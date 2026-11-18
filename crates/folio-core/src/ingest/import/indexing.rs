//! 格式解析、封面资产与导入登记；失败只撤销本次拥有的发布物。

use crate::error::{Error, Result};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
/// 落库那几句的**测试内注错口**：`which` 是这一句的名字，`call` 是真的那一句。
///
/// 为什么非要它：§一 步 7 那条「库里有这一行就等于能打开」全靠失败时把上一步退回去，
/// 而那几种失败全都是「库说失败」—— `Store` 自己没有注错入口（`add_book` 在一个事务里，
/// 从外面造不出半截状态），所以不假造就钉不住「这一段确实调了 [`unwind_landing`]」。
/// 生产编译里 `#[cfg(not(test))]` 那一份就是直接调用，一句多余的话都不留。
#[cfg(not(test))]
pub(super) fn landing<T, E: Into<Error>>(
    which: &'static str,
    call: impl FnOnce() -> std::result::Result<T, E>,
) -> Result<T> {
    let _ = which;
    call().map_err(Into::into)
}

// [`landing`] 的测试那一版把旗子按**线程**放：一批并行时真正导书的是 worker 线程，
// 而进程级的旗会把同时在跑的别的测试一起带红（本仓库的并行污染有先例）。钉这一格的
// 测试因此都把并行度设成 1 —— 那时 [`Batch::run`] 就在调用线程上跑，这一格才够得着。
#[cfg(test)]
thread_local! {
    static FAIL_LANDING: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// 让落库的某一句在下一次假失败（`None` = 收回去）。**只在测试里存在**，
/// 而且只对调用这条线程有效（见上面那段）。
#[cfg(test)]
pub(crate) fn fail_landing(which: Option<&'static str>) {
    FAIL_LANDING.with(|slot| slot.set(which));
}

#[cfg(test)]
pub(super) fn landing<T, E: Into<Error>>(
    which: &'static str,
    call: impl FnOnce() -> std::result::Result<T, E>,
) -> Result<T> {
    if FAIL_LANDING.with(|slot| slot.get()) == Some(which) {
        return Err(Error::Io {
            path: PathBuf::from(which),
            source: "测试按下去的假失败".to_string().into(),
        });
    }
    call().map_err(Into::into)
}

/// 把一份文档的封面写进 `<书库>/covers/`，并把描述填回 `doc.cover_ref`。
///
/// 三条规矩（`plans/导入管线.md` §八 第 4 条）：
///
/// - **按封面自己的字节哈希命名**，不是书的哈希：两本书共用一张封面时磁盘上
///   只有一个文件；重新解析或换 provider 也不该让封面改名。
/// - **已存在就不写**：重导同一本书是常见事（系列书、重扫整库），
///   哈希相同 = 字节相同，重写一遍只是白付一次 IO。这条让「重扫整库」安全。
/// - **失败只记日志，不回 Err**：封面是装饰。写不成时 `cover_ref` 留 `None`，而库里
///   再也没有字节可退（v16 删了 `books.cover` 那一列），结果就是这本书没有封面、
///   界面画占位。
///
/// 两族都走这一条。从前它开头挡了一句「只处理可重排」，理由写的是「定版式的封面走
/// `save_fixed`」—— 而 `save_fixed_in` 里根本没有那两列，于是**整族的封面算完就丢**：
/// 漫画一直是这样，PDF 接上首页栅格之后也会一样。现在两族写的是同一张
/// `covers/<hash>.<ext>`、同一对 `cover_hash` / `cover_mime` 列，界面不再问格式。
pub(super) fn land_cover(root: &Path, doc: &mut dyn crate::content::document::Document) {
    let Some(bytes) = doc.cover().map(<[u8]>::to_vec) else {
        return;
    };
    if bytes.is_empty() {
        return;
    }
    if let Some(cover) = write_cover(root, &bytes) {
        doc.set_cover_ref(Some(cover));
    }
}

/// 把一份封面字节写进 `covers/` 并生成缩略图，交回描述；失败回 `None`。
///
/// 拆出来给「封面补齐」那一趟共用：它是同一份字节、同一个文件命名、同一对列，
/// 不该在维护作业里抄第二遍。失败只记日志，不回 `Err` —— 封面是装饰。
pub(super) fn write_cover(
    root: &Path,
    bytes: &[u8],
) -> Option<crate::content::assets::cover::CoverRef> {
    let mime = cover_mime(bytes);
    let written = match crate::content::assets::cover::write(root, bytes, &mime) {
        Ok(hash) => hash.to_hex(),
        Err(err) => {
            // 字节不进库（v16），所以这一次失败就是「这本书没有封面」——
            // 留着 `cover_ref` 为 `None`，界面画占位。不阻断导入：封面是装饰。
            tracing::warn!(
                path = %root.display(),
                "import: 封面没写进 covers/，这本书将没有封面：{err}"
            );
            return None;
        }
    };
    let cover = crate::content::assets::cover::CoverRef {
        hash: written,
        mime,
    };
    // **缩略图在落盘时一起生成**（`plans/导入管线.md` §八「缩略图」那一段推荐的正是
    // 这一档）。这一版能这么选，是因为字节不再进库：封面只有一个来源（`covers/` 那个
    // 文件），所以「落盘时生成」不会与「懒生成」抢同一份输入。
    //
    // 失败不阻断，也不记 warn：做不出来的那些格自己回 `None`，
    // 而它失败的原因（原图是坏字节）对读者来说与「这本书本来就没有封面」同一种表现。
    //
    // 用 `ensure_all` 而不是逐档 `ensure`：同一份封面字节只该解一次图（实测省下的
    // 那一次解码约 3-7 ms/本，而两档的产出逐字节不变）。
    crate::content::assets::thumb::ensure_all(root, &cover, bytes);
    Some(cover)
}

/// 封面的 MIME：**只看字节头**。
///
/// 这一格只决定封面文件叫什么扩展名，不参与任何判读，所以不值得为它去问 manifest：
/// 真书里 `image/jpg`、`IMAGE/PNG` 这类声明写错比字节头写错常见得多（`ui/shared/images.rs`
/// 认图片格式就是因为这个才把字节头排在声明前面）。判据复用既有那一份
/// （`format::guess_media_type`），认不出就按 JPEG —— 它是封面最常见的编码，
/// 而认错扩展名最坏的后果是文件管理器双击用错程序打开。
fn cover_mime(bytes: &[u8]) -> String {
    // 复用既有的那份判据（`format::guess_media_type`，与 `ui/shared/images.rs`
    // 认图片格式同源）：封面这一格只影响文件扩展名，认不出就按 JPEG ——
    // 它是封面最常见的编码，而认错扩展名最坏的后果是文件管理器双击用错程序打开。
    crate::content::format::guess_media_type(bytes)
        .unwrap_or("image/jpeg")
        .to_string()
}
