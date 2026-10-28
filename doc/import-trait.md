# 导入管线：一条抽象，七种来源

> 状态：**本地全套 + URL 下载 + Wi-Fi 传书已落地**（M1–M7，2026-10-11）。
> WebDAV 双向同步（M8）只有设计、没有实现 —— 形状定在第五节，实现时不需要改 trait。
> 前置：`plans/background-pipeline.md` 那条 task 管线；本文是在它之上换的形状。

## 为什么换形状

上一版的签名是 `import_books(sources: &[PathBuf], dest_dir)` —— 「候选就是一批本地
路径」写进了签名，落库还留在界面线程（`rusqlite::Connection` 非 `Sync`）。要接扫描、
监听、网络导入，这两条都得动：

- **候选**从哪来（列目录 / PROPFIND / 等上传 / 等下载）是各来源自己的事，要给它
  进度与取消的口子；
- **落库**要进管线：扫一个 300 本的目录 = 300 × 几十毫秒的界面占用，而 `Store`
  内部加一把锁就能让它在后台线程直接写。

## 一、抽象

只抽象**「一本书的出处 → 书库里那份本地副本」**。拷进书库、解析、登记、落章节
全是管线自己的事，逐本进行、逐本报数：

```rust
pub enum ImportKind { Picked, Paths, Scan, Watch, Transfer, Url, WebDav }

pub enum Origin { Local(PathBuf), Remote { url: String, path: String, size: Option<u64> } }
pub struct Candidate { origin: Origin, name: String, size: Option<u64> }

pub enum NameClash { Overwrite, Suffix }
pub struct Policy { skip_existing: bool, copy_in: bool, on_name_clash: NameClash }

pub trait Source: Send + Sync + 'static {
    fn kind(&self) -> ImportKind;
    fn policy(&self) -> Policy;
    fn candidates(&self, dest_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>>;
    fn stage(&self, c: &Candidate, dest_dir: &Path, ctx: &StageCtx) -> Result<PathBuf>;
}

pub struct Registered { book_id: i64, dest: PathBuf, title: String }
pub struct ImportOutcome { candidate: Candidate, result: Result<Option<Registered>> }

pub fn import_from(source: &dyn Source, dest_dir: &Path, store: &Store, ctx: &StageCtx)
    -> Vec<ImportOutcome>;
pub fn submit_import(manager, source: Box<dyn Source>, dest_dir: PathBuf, store: Arc<Store>)
    -> TaskHandle<Vec<ImportOutcome>>;
```

要点：

- **逐本 `stage → parse → register`，顺序是硬的**；单本坏了只留自己的 `Err`。
- **取消在书与书之间检查**；`Cancelled` 时**已经落库的那些是真的** —— 所以架面上
  那颗「取消」按钮做得了（上一版做不了：`Cancelled` 不带结果，丢的是没落库的解析）。
- `ImportOutcome` 不再揣 `Box<dyn Document>`：逐本落完即释放，300 本不再攒一整批正文。
- 跳过判定按**副本路径**（`books.path` 是书的身份）；`NameClash::Suffix` 用**文件大小**
  判同名不同字节（宁可多一格，不覆盖；逐字节比对在几千文件的扫描里不值）。
- `copy_in = false` 留给「书已在设备上、就地登记」的入口（阅读页那类），现在没有来源用。

## 二、`Store` 变 `Sync`：锁加在连接上

`conn: Connection` → `conn: Mutex<Connection>`，`Store: Send + Sync`（编译期断言钉在
struct 旁边）。公开方法只「上锁一次 + 调 `*_in(&conn, ..)` 私有助手」—— 五个拼接方法
（`shelf_grouped` / `collection_groups` / `save_content` / `save_document` / `load_content`）
不拆就会自锁。界面侧 `Rc<Store>` → `Arc<Store>`。

配套纪律写在两边模块文档：**stage 与 parse 绝不持锁**，只有 register 那一小段持 ——
后台导一本书时界面最多等几十毫秒。

## 三、`task` 整形：非泛型 `StageCtx`

`Progress<R>` 的泛型绑在结果类型上，`dyn Source` 里写不出来。换成非泛型的
`StageCtx`（装箱闭包 + 取消旗标，`R` 不出现在签名上）：

- `item(done, total)` —— 整批读数（`total = 0` 即不定进度，扫描用它报「已找到 N 本」）；
- `bytes(done, total: Option<u64>, name)` —— 字节级读数（下载用），`TaskEvent::Bytes`
  独立变体，与 `Progress` **互不覆盖**（测试钉住）；
- `message()` —— 只进日志：core 的文案不走界面（locale 在 app 侧组）；
- `cancel()` —— 协作式取消的旗标，`TaskHandle::cancel` 与它共享同一面。

## 四、七种来源各自落在哪

| 来源 | candidates | stage | 现状 |
| --- | --- | --- | --- |
| 选文件 `Picked` | 选择器交回的路径 | `fs::copy`（同名即覆盖） | ✅ |
| 批量路径 `Paths` | 清单勾选 / 传书落盘 | 同上 | ✅（勾选与传书走它） |
| 整机扫描 `Scan` | 走目录，只收认识的扩展名 | 同上（Suffix） | ✅ |
| 监听 `Watch` | inotify 事件攒批 | 同上（skip + Suffix） | ✅ |
| Wi-Fi 传书 `Transfer` | 服务器收到的每份上传 | 同上 | ✅ |
| 链接下载 `Url` | 一条 URL | 流式 GET 落副本 | ✅ |
| WebDAV `WebDav` | PROPFIND ∩ 需要下载的条目 | DAV 下载落副本 | 定形未实现 |

### 整机扫描（`scan.rs`）

- roots 平台给：桌面**家目录**（`/` 全盘没有书还慢）、Android 共享存储根
  （本应用外部目录在 `Android/data` 底下，是噪音豁免区）、iOS / 鸿蒙数据根。
  core 只认注入的 roots。
- 跳过：点开头 + 噪音清单（`node_modules` / `AppData` / `Library` / `Android/{data,obb}`
  逐层判、豁免区放行）+ **书库目录自己**；EACCES 计数不中断。
- 预算：深度 8、访问 200k、候选 5000，到顶置 `truncated`（深度剪枝是策略、不算截断）。
- **只找不导**：家目录里 `.txt`/`.md` 多半是 README，全量直导会把书架灌满杂物。
  找到 → 清单页按目录分组、复选框（已在书架的标出且默认不勾）→ 导入所选走
  `PathsSource::picked`（勾选 = 亲手挑的，重名算更新）。
- Android 权限：fork `gpui-pre-mobile` `29f19d3` 给 permission_handler 增加了
  `Permission::ManageExternalStorage`（「所有文件访问」，参考 Godot `PermissionsUtil`：
  检查走 `isExternalStorageManager()`，请求带读者去系统设置页、阻塞到回来）。
  扫描前在**后台线程**上请求（`ui::platform::request_storage_access`），要不到就
  退回只扫应用自己目录；没授权时读不了的地方计入清单页的「跳过 N 处」。

### 监听（`watch.rs`，依赖 `notify`）

- 服务 = 初扫（复用 `scan::find`）+ `notify::RecommendedWatcher`（inotify），
  起不来退 `PollWatcher`（间隔取配置）。
- 放行判据：**相邻两轮 tick（1s）尺寸一致**（防半拷贝）+ **库里还没有**
  （`book_by_path`）+ 深度内 + 认识的扩展名。攒批 → `PathsSource`（Watch 语义）。
- 配置 `library.watch: Option<WatchDir{ dir, interval_secs }>`；监听书库目录自己
  会被 normalize 当没开（自我循环）。生命周期挂 app 层 `ui::shared::watch`：
  boot 起、配置改一次换一只，活过界面切换。

### Wi-Fi 传书（`transfer/`，tokio + axum **只住这里**）

- 设备起 HTTP 服务器，局域网浏览器开 `http://<IP>:<port>/?pin=XXXXXXXX` 上传；
  八位配对码每次启动随机（熵走 `RandomState` 的 OS 种子，不是时钟低位），错码 403，
  同一地址连错六次之后按指数退避吃 429 + `Retry-After` —— 陌生设备最多看见一个上传页，
  也试不出几个码。配对码不落日志（会进「反馈日志」那份文件）。
- multipart **流式**落收件箱（块进块写），文件名在进收件箱前剥掉一切路径成分
  （路径穿越在这里掐死）；每个文件完成回调 `on_file` → `PathsSource`（Transfer
  语义）进导入队列，**成功后清收件箱原件**。
- 执行器边界：Runtime 自建、跑在 `folio-transfer` 线程上、对外只交同步 API，
  core 公共签名不漏 `Future`。「core 除 transfer 外不依赖异步运行时」写进模块
  文档；将来嫌软可拆独立 crate。
- 传书服务**不写进配置**：开到关掉为止，重启不会自己开（「我在局域网里裸着」
  这个状态不该瞒着读者自己恢复）。

### 链接下载（`UrlSource`）

- 名字取 URL 路径末段（百分号解码、剥掉 scheme://authority 与查询串 —— 签名
  直链的末段通常还是书名）；`stage` 流式 GET 落成副本（`.part` 临时名 → `finish`
  改名 / `abort` 清掉，与 `Transport::download` 的 finish/abort 契约同句）。
- `Transport` 同步化（`request` / `download(&mut dyn Sink, &Cancel)`），实现
  `UreqTransport`（`ureq` + rustls + webpki-roots；401 + Digest 挑战单独一支
  错）。出网与传书是两套 HTTP 栈（ureq 出、axum 收），互不复用。
- 界面：书架「链接导入」按钮 → 内联面板（与书库确认面板同一套样式）粘贴链接。

## 五、定形未实现：WebDAV 双向同步（M8）

- **`WebDavSource` 留在导入管线内**（是 `Source` 的一种）：「远端一本书 → 书库里
  那份副本」就是导入的定义，逐本循环不该有第二份。**同步编排器在管线外**
  （`remote/sync.rs`）：PROPFIND + diff → 要下载的交给管线 → 拿 `ImportOutcome`
  回填状态表；上传 PUT 与删除传播自己干。**管线是地基，监听 / 传书 / 同步都是
  往队列塞活的驱动者。**
- 状态表（迁移 v8）：`sync_state(name PK, etag, size, local_size)`，定位是
  「我们以为同步到哪了」的**缓存** —— 漏记的下轮重下重记，代价只是重复一次下载。
- 双向语义：本地新增 → PUT；单方变化 → 按变化方向走；双方都变 →
  `SyncPolicy{ conflict: RemoteWins | LocalWins | Skip, propagate_deletions: bool }`。
  删除传播必须靠状态表分辨「本地删了」与「远端新来的」—— 没有它，「远端不在」
  会把本地所有书误判成待删。
- 凭证：config.json 增 `webdav{ url, username, password, root, sync }`，明文、
  集中一个 struct，将来整体平移钥匙串。
- `dav::Client` 已随 `Transport` 同步化（`dav.rs` 的协议用例一行没改语义）。

## 六、线程模型（本方案的立场）

全方案都是真多线程：`task` 线程池（`std::thread` = OS 线程，真并行）、扫描线程
（`scan_queue`，独立于导入队列）、监听线程（`folio-watch`）、传书线程
（`folio-transfer` 里的 tokio runtime）各管各的。std 线程（阻塞式）适合「任务少、
逐个阻塞活」的管线形状；传书服务器选 tokio 不是为了多线程能力，而是「少量线程
轮转服务大量连接」的现成调度与 multipart 的现成流式解析 —— 自写这套解析历来是
安全洞高发区。

## 七、测试清单（已落地）

- core 482 条全绿。新增的关键门：
  - 管线：picked 登记上架（书目+章节）/ 重导同路径=同行 / skip_existing 跳过 /
    Suffix 落 `-2` / 两处取消语义（预触发不碰、书间取消已入库的都在）；
  - task：item 与 bytes 两条读数互不覆盖；
  - scan：找到与跳过 / 权限计数 / 深度与候选预算 / 噪音豁免 / `supports_extension`
    与注册表一致；
  - watch：Collector 判批（半拷贝不放行）/ 真 watcher 端到端自动入库；
  - url：名字规则 / 假传输端到端（落盘、取消无残留）；
  - transfer：路径穿越 / ureq 手拼 multipart 端到端（错码 403、落盘、回调）。
- app 72 条全绿。`shell::booted` 两扇门改走新签名：导入（Registered 的 id 让架面
  重查、封面缓存被清）与扫描（清单勾选 → 导入所选接上管线）。
- `library.rs` 的两扇删除门不动；原「落库落全」的门随落库搬进了 core。

## 还没做的

- M8 WebDAV 双向同步（第五节，形状已定）。
- 扫描清单的大列表目前渲染上限受虚拟列表保护，5000 条以外的体验没打磨。
- 导入失败的界面呈现（现在只有日志，读者只知道「少了几本」）。
- 监听自动导入目前不打扰读者（事件只进日志、架上多一本）；要接读数的话把
  `submit_import` 的句柄交 `forward_to_auto_imports`（传书已经这么接了）。
