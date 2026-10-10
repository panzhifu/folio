# 后台任务管线与导入管线

> 状态：**后端（folio-core）已完成**；应用层还没接（`folio-app` 当前因未完成的模块搬迁编不过，
> 见文末）。两个模块都在 core，不依赖 gpui，`cargo test -p folio-core` 全量覆盖。

## 为什么

导入以前是**同步跑在 UI 线程**上的（`shelf/view.rs::import_step`）：拷文件 → 解析 → 写库，
五步一路 `?`。解析一本 EPUB 是几十到几百毫秒，一次选十本就是几秒的卡顿；而这几件事本来
就不需要窗口。于是把「可取消、可报进度的作业丢到后台」这件事做成一条通用管线，导入是它的
第一个用户。

## 一、通用后台任务管线：`folio_core::task`

只依赖 `std`（`thread` / `mpsc` / `Condvar`），不引 tokio / crossbeam，与 core 其余部分一致。

形状是「**一个作业一条通道**」：每次 `submit` 新开一条 `mpsc`，事件不共用、不带 id 路由 ——
交出去的 `TaskHandle` 就是这条作业的全部接口。

```rust
let manager = TaskManager::<u64>::new(2);          // 2 条工作线程
let handle = manager.submit(|progress, cancel| {   // 在后台线程上跑
    progress.report(1, 3, "读第一段");
    if cancel.triggered() { /* 尽快返回 */ }
    Ok(42)
});
for event in handle.events {                        // Started / Progress* / Finished|Cancelled
    // 界面在这里更新
}
```

要点：

- **进度**：`Progress::report(done, total, message)`；`total == 0` 表示不定进度。
- **取消**：沿用 `remote::Cancel`（置位、由作业在检查点自己看见的**协作式**取消，不抢占，
  最多晚一个检查点）。作业返回时若已置位，一律落 `TaskEvent::Cancelled`。
- **结果**：`TaskEvent::Finished(Result<R, TaskError>)`，`TaskError` 是装箱错误
  （`Box<dyn Error + Send + Sync>`，`anyhow::Error` 就是它，界面侧收因果链不用再包一层）。
- **队列**：`Mutex<VecDeque>` + `Condvar`。**不用 std 的 `mpsc` 当队列** —— 它的接收端不是
  `Sync`，多线程只能有一个 `recv`，工作线程数会退化成 1。
- **关停**：`shutdown()` 不再等新活、把已在队里的跑完、回收线程；`Drop` 会调它。
- `TaskManager<R>` 的 `R` 只体现在 `submit` 的签名上，运行期不需要，用 `PhantomData` 挂着。

## 二、导入管线：`folio_core::import`

建在 `task` 之上，只做两件与格式无关的事：**拷进书库目录**、**解析**。

```rust
let manager = TaskManager::<Vec<ImportOutcome>>::new(1);
let handle = submit_import(&manager, sources, dest_dir);
// ...
for event in handle.events {
    if let TaskEvent::Finished(Ok(outcomes)) = event {
        for outcome in outcomes {
            match outcome.result {
                Ok(book) => { /* 调用方 add_book + save_document */ }
                Err(e)   => { /* 这一本坏了，不影响别的 */ }
            }
        }
    }
}
```

要点：

- **逐源各自成败**：一批里坏掉的那一本只影响它自己（`ImportOutcome::result` 是一条 `Err`），
  不把整批拖垮。对应旧流程里「一次选多本是常态」的取舍。
- **进度**：每开始一个源报一次 `done/total`（配「导入《名字》」），收尾再报一次完成。
- **取消**：在**源与源之间**检查；单个源内部（拷贝 / 解析）不可中断。
- **落库留在调用方**：`Imported` 里带着解析好的 `Box<dyn Document>` 与副本路径，
  `Store::add_book` / `save_document` 由调用方做 —— `Store` 里的 `rusqlite::Connection` 是
  `Send` 但**不是** `Sync`，搬到工作线程要么挪走要么套锁，而写库是毫秒级的一步，
  留在它本来的线程上最干净。管线的职责到此为止：把最贵的拷贝与解析挪到后台。

## 测试（`cargo test -p folio-core`，新增 8 条）

- `task::*`：作业跑起来 + 进度 + 结果、失败带回错原文、取消落 `Cancelled`、
  一批作业都完成（4 条线程）、关停回收线程。
- `import::*`：逐源拷贝 + 解析、坏源不拖垮整批、一上来取消则一个源都不碰。

## 还没做的（应用层）

- `folio-app` 编不过（未完成的模块搬迁：`features/reader/mod.rs` 没声明 `mod document; mod measure;`，
  `view.rs` 还 import 旧的 `shared::document` / `shared::measure`，且 `book_fonts` 模块缺失、
  `BookImages::new` 少一个参数）。所以**核心管线已备好，但书架还没改成调它**。
- 接法（下一步）：`ShelfView` 持一个 `TaskManager<Vec<ImportOutcome>>`；选文件后
  `submit_import`，每帧读事件更新进度条，`Finished` 后对每条成功的 `Imported` 调
  `store.add_book` + `save_document` 并刷新架面。
