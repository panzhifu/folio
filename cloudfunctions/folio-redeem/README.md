# folio-redeem

爱发电订单号 → 会员天数的云函数。客户端把 `{ "order": "...", "device": "..." }`
POST 到 `/redeem`；`action=release` 走解除。

## 测试订单（只在开发 / 体验环境开）

正式路径只认爱发电真实已付款订单。做端到端实验时，可以让服务端认几个**写死在环境变量里**
的测试订单号；它们走同一条 `folio_redemptions` 通道，所以「一单只发一次、一单只绑一台
设备、解除后可换设备」这些行为与真单完全一样。

三个变量**必须同时**成立，缺一条就完全不起作用：

| 变量 | 值 | 说明 |
| --- | --- | --- |
| `FOLIO_ALLOW_TEST_ORDERS` | `1` | 显式开关，默认关 |
| `FOLIO_TEST_ORDERS` | `90000000000000000000000000001=365` | 订单号=天数，逗号分隔；号必须是 16–40 位数字 |
| `FOLIO_TEST_ORDER_UNTIL` | `2026-11-12T00:00:00Z` | 截止时刻；过期或解析不出来一律不认 |

**不要在生产环境设置这三个变量。** 实验完就把它们删掉（或把 `FOLIO_ALLOW_TEST_ORDERS`
改回 `0`）。测试单会真的写进 `folio_redemptions`，删变量不会回收已经发出去的天数；
要回收就把那一行删掉，或在客户端走一次「解除激活」。

### 在 CloudBase 控制台设置

函数 `folio-redeem` → 函数配置 → 环境变量，加上上面三行，保存后重新部署或等配置生效。
不建议用 `cloudbaserc.json` 的 `envVariables` 整份覆盖：那会把现有的
`AFDIAN_*` / `CLOUDBASE_*` 变量一起冲掉。

### 本地调用

```bash
curl -sS -X POST \
  'https://shocked-roundworm-d1dqjn15d00708-1476411022.ap-shanghai.app.tcloudbase.com/folio-redeem/redeem' \
  -H 'Content-Type: application/json' \
  -d '{"order":"90000000000000000000000000001","device":"test-device-1"}'
```

成功时回 `{"ok":true,"state":"granted","days":365}`。同一台再交一次回 `replay`；
换一台设备回 `bound`；先 `{"action":"release","device":"test-device-1"}` 再换设备回
`moved`（搬剩余天数）。

### 部署

```bash
cd <repo-root>
tcb fn deploy folio-redeem --force
```

`--force` 会覆盖线上代码；环境变量不在这条命令里，先在控制台确认三个测试变量是否还在。
