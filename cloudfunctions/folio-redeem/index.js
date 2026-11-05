// 爱发电订单 → Folio 会员天数。读者在设置页填订单号，这里验单、抢占、报天数；
// 同一支函数也管反方向那一趟（body 里 action=release）：把这台设备名下的行删掉，
// 那些单就能在新设备上重新兑 —— 「换设备」是同一个功能的两半，拆开放就两边都讲不清。
//
// 之所以在服务端验而不是本地：爱发电的 api token 一旦进了客户端就等于公开，
// 任何人都能拿它查你的全部订单。本地只可能校「格式对不对」，校不了「这单真付过钱」。
//
// 一单一次靠 folio_redemptions 的主键：并发双花由 resolution=ignore-duplicates 挡在
// 数据库那侧，而不是靠先查再插 —— 查和插之间没有锁，两个人都能查到「没人用过」。

const crypto = require("crypto");
const http = require("http");
const https = require("https");
const { URL } = require("url");

const ENV_ID = process.env.CLOUDBASE_ENV_ID || "";
const PG_KEY = process.env.CLOUDBASE_APIKEY || "";
const AFDIAN_USER_ID = process.env.AFDIAN_USER_ID || "";
const AFDIAN_TOKEN = process.env.AFDIAN_API_TOKEN || "";

const AFDIAN_QUERY_ORDER = "https://afdian.com/api/open/query-order";
// 表名写死：这段路径不接受调用方拼进来的任何字符。
const PG_ROWS = `https://${ENV_ID}.api.tcloudbasegateway.com/v1/rdb/rest/folio_redemptions`;

// 订单号是 29 位数字。形状不对直接拒，省掉一次上游请求，也不给日志喂脏数据。
const ORDER_SHAPE = /^[0-9]{16,40}$/;

function planDays() {
  // AFDIAN_PLAN_DAYS 形如 "<方案id>=365,<另一个id>=30"，逗号分隔。
  //
  // 不用 JSON 是实测挡出来的：值里一带花括号，`tcb config update fn` 就把它当对象送上去，
  // 回 `Environment.Variables.N.Value ... input type should be string`（2026-11-05），
  // 于是这张表只能到控制台里手填。写成没有分隔歧义的 kv，整条配置就还能留在脚本里。
  //
  // 认不出任何一条就当没配 —— 宁可拒所有单，不要因为一个手抖的引号把免费单当成付费单放行。
  const table = {};
  for (const pair of (process.env.AFDIAN_PLAN_DAYS || "").split(",")) {
    const at = pair.indexOf("=");
    if (at < 1) continue;
    const id = pair.slice(0, at).trim();
    const days = Number(pair.slice(at + 1));
    if (id && Number.isInteger(days) && days > 0) table[id] = days;
  }
  return table;
}

// 这单该兑几天。id 在订单里有两个可能的位置：方案单在 plan_id，单商品（专辑）
// 在 sku_detail[].album_id（官方示例里后者的 plan_id 是空串）。只认 plan_id 的话，
// 商品那一类全部落到「不是会员方案」—— 那是收了钱不给货。
function matchPlan(order) {
  const table = planDays();
  const ids = [order.plan_id, ...(order.sku_detail || []).map((s) => s.album_id)].filter(Boolean);
  for (const id of ids) {
    const days = Number(table[id]);
    if (Number.isInteger(days) && days > 0) {
      return { id, days };
    }
  }
  return null;
}

function json(res, code, body) {
  res.writeHead(code, { "Content-Type": "application/json; charset=utf-8" });
  res.end(JSON.stringify(body));
}

function readBody(req) {
  return new Promise((resolve) => {
    let raw = "";
    req.on("data", (chunk) => {
      raw += chunk;
      // 这个接口只该收三个短字段（order / device / action）。超了就是有人在灌，
      // 当场拒掉而不是继续缓冲。
      if (raw.length > 4096) {
        req.destroy();
        resolve(null);
      }
    });
    req.on("end", () => {
      try {
        resolve(raw ? JSON.parse(raw) : {});
      } catch (_) {
        resolve(null);
      }
    });
    req.on("error", () => resolve(null));
  });
}

// 不用全局 fetch：Nodejs18 上它还是实验性的，运行时哪天收紧就整站 500。
function request(method, url, headers, payload) {
  return new Promise((resolve, reject) => {
    const target = new URL(url);
    const req = https.request(
      {
        method,
        hostname: target.hostname,
        port: target.port || 443,
        path: `${target.pathname}${target.search}`,
        headers: { ...headers },
        timeout: 8000,
      },
      (res) => {
        let raw = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          raw += chunk;
        });
        res.on("end", () => {
          let data;
          try {
            data = JSON.parse(raw);
          } catch (_) {
            data = { raw: raw.slice(0, 200) };
          }
          resolve({ status: res.statusCode, data });
        });
      },
    );
    req.on("timeout", () => req.destroy(new Error("上游超时")));
    req.on("error", reject);
    if (payload !== undefined) {
      req.end(payload);
    } else {
      req.end();
    }
  });
}

const JSON_HEADERS = { "Content-Type": "application/json", Accept: "application/json" };

async function postJson(url, body, headers) {
  return request("POST", url, { ...JSON_HEADERS, ...headers }, JSON.stringify(body));
}

async function getJson(url, headers) {
  return request("GET", url, { Accept: "application/json", ...headers });
}

// 爱发电的签名：token + 按 key 排序的 kv 直接拼，没有分隔符，token 本身不上送。
// 顺序是 params → ts → user_id（三份参数名排序后的自然次序）。
function afdianSign(params, ts) {
  return crypto
    .createHash("md5")
    .update(`${AFDIAN_TOKEN}params${params}ts${ts}user_id${AFDIAN_USER_ID}`)
    .digest("hex");
}

async function queryOrder(orderNo) {
  const params = JSON.stringify({ out_trade_no: orderNo });
  const ts = Math.floor(Date.now() / 1000);
  const { data } = await postJson(AFDIAN_QUERY_ORDER, {
    user_id: AFDIAN_USER_ID,
    params,
    ts,
    sign: afdianSign(params, ts),
  });
  if (data.ec !== 200) {
    // 400004/400005 是 token 或签名的问题，属于我方配置坏了，不是读者的单有问题。
    return { error: "upstream", ec: data.ec };
  }
  const list = (data.data && data.data.list) || [];
  return { order: list.find((o) => o.out_trade_no === orderNo) || null };
}

async function claim(row) {
  const res = await postJson(
    PG_ROWS,
    row,
    {
      Authorization: `Bearer ${PG_KEY}`,
      Prefer: "return=representation,resolution=ignore-duplicates",
    },
  );
  const inserted = Array.isArray(res.data) ? res.data.length > 0 : false;
  return { inserted, raw: res.data, status: res.status };
}

async function lookUp(orderNo) {
  const url = `${PG_ROWS}?select=device,granted_days,redeemed_at,released_at&out_trade_no=eq.${encodeURIComponent(orderNo)}`;
  const res = await getJson(url, { Authorization: `Bearer ${PG_KEY}` });
  return Array.isArray(res.data) && res.data.length ? res.data[0] : null;
}

// 那一单还剩几天。按**当初发货那一刻**（`redeemed_at`，表里本来就有这一列）往下数整日，
// 剩下的就是该搬过去的。
//
// 为什么服务端算而不是把 `redeemed_at` 交给客户端：客户端只有「本地日序号」这一个日历，
// 而 TIMESTAMPTZ 回的是 ISO 串 —— 要么在 core 里再写一个日期解析，要么按 UTC 猜一次偏移。
// 在这一层算完只送一个整数，两端各自少一处能错的地方。
//
// 这一条同时也是「解除 → 重兑」刷不出天的另一半：remaining 永远从**同一个** redeemed_at
// 往下数，转几轮只会更少，不会多。
function daysLeft(row, at) {
  const granted = Number(row.granted_days) || 0;
  const since = Math.floor((at - Date.parse(row.redeemed_at)) / 86400000);
  if (!Number.isFinite(since)) return 0;
  return Math.max(0, granted - Math.max(0, since));
}

// 把已经**软解除**的那一行认领到新的那一台：换 device、抹掉 released_at，**天数一个都不加**。
async function takeOver(orderNo, device) {
  const url = `${PG_ROWS}?out_trade_no=eq.${encodeURIComponent(orderNo)}`;
  const res = await request(
    "PATCH",
    url,
    { Authorization: `Bearer ${PG_KEY}`, Prefer: "return=representation", Accept: "application/json" },
    JSON.stringify({ device, released_at: null }),
  );
  return { status: res.status, rows: Array.isArray(res.data) ? res.data.length : 0 };
}

// 软解除：把这台名下的行标上 released_at，而不是删掉。
//
// **为什么不删**：删了就没有「这一单兑过」的痕迹，而兑换那一段的全部防重靠的就是主键撞上
// 一行旧记录 —— 行没了，同一单再兑一次就又是一整笔天数。解除 → 重兑 → 再解除，一轮几分钟，
// 一单 365 天可以刷成无限。软解除让那一单永远只发一次货，之后只能被**认领**（换设备，
// 搬的是剩下的那几天）。
async function release(device, at) {
  const url = `${PG_ROWS}?device=eq.${encodeURIComponent(device)}&released_at=is.null`;
  const res = await request(
    "PATCH",
    url,
    { Authorization: `Bearer ${PG_KEY}`, Prefer: "return=representation", Accept: "application/json" },
    JSON.stringify({ released_at: at }),
  );
  return { status: res.status, rows: Array.isArray(res.data) ? res.data.length : 0 };
}

async function releaseFlow(res, body) {
  if (!PG_KEY || !ENV_ID) {
    return json(res, 200, { ok: false, state: "unconfigured" });
  }
  const device = String(body.device || "").trim();
  if (!device || device.length > 128) {
    return json(res, 200, { ok: false, state: "bad_device" });
  }
  const done = await release(device, Date.now());
  if (done.status >= 400) {
    // 只打状态码：那串 device 是读者的安装标识，没必要进日志。
    console.log(`release 写库失败 status=${done.status}`);
    return json(res, 200, { ok: false, state: "storage" });
  }
  if (done.rows === 0) {
    // 与「这单绑在另一台」同一句：这一屏本来就不该告诉调用方别人名下有什么。
    return json(res, 200, { ok: true, state: "nothing_bound", released: 0 });
  }
  return json(res, 200, { ok: true, state: "released", released: done.rows });
}

async function redeem(req, res, body) {
  // 一条路径两种动作，靠 body 里那一格分流：网关登记的 HTTP 访问路径只有
  // `/folio-redeem/redeem` 这一条，而客户端那份 ENDPOINT 是写死的（开成一格配置等于把
  // 「订单号发给谁」交给读者填，理由见 `net/redeem.rs`）。加一条路径要动环境的路由与安全
  // 域名，两端各多一处要改的地方；不带 action 的旧客户端照旧走兑换。
  if (String(body.action || "").trim() === "release") {
    return await releaseFlow(res, body);
  }
  if (!AFDIAN_USER_ID || !AFDIAN_TOKEN || !PG_KEY || !ENV_ID || Object.keys(planDays()).length === 0) {
    // 配置没齐就把单判成无效会误导读者；这里如实说服务端还没准备好。
    return json(res, 200, { ok: false, state: "unconfigured" });
  }
  const orderNo = String(body.order || "").trim();
  const device = String(body.device || "").trim();
  if (!ORDER_SHAPE.test(orderNo)) {
    return json(res, 200, { ok: false, state: "bad_code" });
  }
  if (!device || device.length > 128) {
    return json(res, 200, { ok: false, state: "bad_device" });
  }

  const found = await queryOrder(orderNo);
  if (found.error) {
    console.log(`query-order 失败 ec=${found.ec}`);
    return json(res, 200, { ok: false, state: "upstream" });
  }
  const order = found.order;
  if (!order) {
    return json(res, 200, { ok: false, state: "not_found" });
  }
  // 平台只把交易成功的单推给 webhook，但查询 API 会把待付/退款的也带回来。
  if (order.status !== 2) {
    return json(res, 200, { ok: false, state: "unpaid" });
  }
  const matched = matchPlan(order);
  if (!matched) {
    return json(res, 200, { ok: false, state: "plan" });
  }

  // `redeemed_at` 由表自己的 DEFAULT now() 填 —— 那是「这一单第一次发货」的那一刻，
  // 之后认领过去时按它数还剩几天，所以这一列**只在第一次写**，谁都不许再改它。
  const claimed = await claim({
    out_trade_no: order.out_trade_no,
    device,
    plan_id: matched.id,
    buyer_id: order.user_private_id || order.user_id || "",
    granted_days: matched.days,
    paid_amount: order.total_amount,
  });
  if (claimed.status >= 400) {
    console.log(`claim 写库失败 status=${claimed.status}`);
    return json(res, 200, { ok: false, state: "storage" });
  }
  if (claimed.inserted) {
    return json(res, 200, { ok: true, state: "granted", days: matched.days });
  }

  // 主键撞了：这单被谁用过、是不是同一台设备、有没有被软解除过，回读那行才知道。
  const existing = await lookUp(order.out_trade_no);
  if (!existing) {
    // 撞了主键却读不回来 —— 库在骗我们。宁可报「暂时不可用」，也不要悄悄回一句「已经兑过了」：
    // 那一趟读者到底有没有拿到货，这里说不清。
    console.log("claim 撞了主键但回读为空");
    return json(res, 200, { ok: false, state: "storage" });
  }
  if (existing.device === device) {
    // 同一台设备重交（网断了点两次）：不重复加天数，但也不报错 —— 权益第一次就给了。
    return json(res, 200, { ok: true, state: "replay", days: 0 });
  }
  if (existing.released_at !== null && existing.released_at !== undefined) {
    // 软解除过：这一单可以从新那一台认领。搬过去的是**剩下的那几天**，一次都不多给。
    const left = daysLeft(existing, Date.now());
    if (left === 0) {
      // 时间已经走完：那一行原样留着（搬过去也只是把「过期」挪个名字），如实说一句。
      return json(res, 200, { ok: false, state: "spent" });
    }
    const moved = await takeOver(order.out_trade_no, device);
    if (moved.status >= 400 || moved.rows === 0) {
      console.log(`takeOver 写库失败 status=${moved.status}`);
      return json(res, 200, { ok: false, state: "storage" });
    }
    return json(res, 200, { ok: true, state: "moved", days: left });
  }
  return json(res, 200, { ok: false, state: "bound" });
}

const server = http.createServer(async (req, res) => {
  if (req.method === "OPTIONS") {
    // 走网关时 CORS 头由网关按环境安全域名填，这里自己加会变成 `<origin>,*` 而被拒。
    return res.writeHead(204).end();
  }
  const url = new URL(req.url || "/", "http://127.0.0.1");
  if (req.method === "GET" && url.pathname === "/health") {
    return json(res, 200, { ok: true });
  }
  if (req.method === "POST" && url.pathname === "/redeem") {
    const body = await readBody(req);
    if (!body) {
      return json(res, 200, { ok: false, state: "bad_body" });
    }
    try {
      return await redeem(req, res, body);
    } catch (err) {
      // 只打 message：err 上挂着的是带 Bearer 头的请求对象，整包进日志等于把密钥进日志。
      console.log(`redeem 异常 ${err && err.message}`);
      return json(res, 200, { ok: false, state: "internal" });
    }
  }
  if (req.method !== "GET" && req.method !== "POST") {
    return json(res, 405, { ok: false, state: "method" });
  }
  return json(res, 404, { ok: false, state: "route" });
});

server.listen(9000);
