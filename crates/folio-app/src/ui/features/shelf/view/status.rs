//! 导入那一张记录卡：一行一本书。
//!
//! 卡头**只有一行标题**：成功几本 / 没进来几本不再在这儿重数一遍 —— 逐行自己带状态，
//! 汇总与逐行说的是同一批账，摆两处只是让读者数两次。在跑那一档的进度同样交给每本
//! 名字下面那条按字节走的进度条（`ByteProgress::name` 本来就带书名）。
//!
//! 一行里的主次：书名是唯一焦点（中字重、占满剩余宽），结清那一档里**要读者处理**
//! 的那几类（没进来 / 要口令）做成右侧一枚**语义色标签**，「成功 / 已在架上」不占标签位
//! —— 它们落进 `状态 · 日期 · 格式 · 大小 · 时长` 那一行小字的头一个词，「打开」是行尾
//! 的次要动作。一批跑完，卡从「在跑」换成「结清」，读者要答的那三问 —— 哪几本进来了、
//! 哪几本没进、为什么 —— 在同一屏里看完，而屏上只有书名一个视觉焦点。
//!
//! 从前那两句摆在顶栏与格子之间、靠左上角，各占一行：读数每报一次就把整片架子往下顶一行，
//! 而它说的事离左上角最远。现在它是屏幕正中那一张卡，与建组、链接导入、扫描结果同一个容器
//! —— 同一片架面上冒出来的浮层不该有两种停靠。
//!
//! 行是从**字节读数**长出来的，不是从「我提交了哪几个文件」预摆的：后者要在界面上重数一遍
//! 候选（文件名、大小），而那份口径 core 已经有了（`PathsSource::candidates_from`），写第二份
//! 必然漂移。代价说清楚：没报过字节的那些（小文件拷得太快、或正卡在解析）只在结清时出现，
//! 屏幕上不会有一排空转的假进度。
//!
//! 逐本的原因在这张卡上说，指名道姓的那一条在日志里（`ShelfView: 导入《名字》失败`）。

use std::rc::Rc;

use folio_core::{ImportLanding, ImportOutcome, ImportReject, Imported};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, TestSupportExt as _, div, px, size,
};

use super::super::overlay;
use super::scan::fmt_size;
use super::{ShelfEvent, ShelfView};
use crate::ui::components::muted_label;
use crate::ui::services::l10n::{duration, tr};
use crate::ui::{host, services};

/// 一行的高：名字 + 下面那一行（进度条或读数）+ 上下留白。定死，虚拟列表要能滚就得
/// 有一张等高的高度表（与扫描那一屏同一条理由）。
const ROW_HEIGHT: f32 = 76.0;

/// 头顶那一段：只有一行标题 + 上下留白。汇总那一行去掉之后收紧，别在标题下面
/// 留一大块空 —— 报上来的高（[`record_height`]）得跟画出来的对得上。
const HEAD_HEIGHT: f32 = 52.0;

/// 脚下那一段：只有「在跑」那一档有（一颗「取消导入」），结清那一档不给动作。
const FOOT_HEIGHT: f32 = 36.0;

/// 读回来时最多读多少条。与库里封顶那个数一致（`IMPORT_LOG_KEEP`）：库既然只留这么多，
/// 界面就不必再拿一个更小的数去截它第二次。
const LOG_LIMIT: i64 = folio_core::IMPORT_LOG_KEEP;

/// 记录上那一格日期。
///
/// **今天的不标**：这张卡最常见的那一刻就是刚导完，那几行不需要一句「今天」占位；一旦跨了
/// 日界线，读的人要的就是「这是哪天」。今年只写「月-日」，跨年才带年份 —— 与「笔记」那一屏
/// 同一个顾虑（只有月日的话，去年的「10-28」看起来像昨天的）。纯数字，不进文案表。
fn record_date(made_at: i64, today: i64) -> Option<SharedString> {
    let day = folio_core::local_day(made_at, host::local_offset_secs());
    if day == today {
        return None;
    }
    let date = folio_core::civil_from_days(day);
    if date.year == folio_core::civil_from_days(today).year {
        Some(format!("{:02}-{:02}", date.month, date.day).into())
    } else {
        Some(format!("{:04}-{:02}-{:02}", date.year, date.month, date.day).into())
    }
}

/// 这张卡此刻该多宽。
///
/// 上限 420，窄屏让给「视口减掉左右各一档留白」——手机上（384 逻辑像素那一档）照搬上限
/// 会把卡顶成满幅还溢出屏幕，右边那两颗钮整个掉到屏幕外，读者看到的正是「这张卡关不掉」。
/// 留白给两档 `GUTTER`：与架面第一格那条对齐脊线同一个数，卡不会贴到屏幕边。
pub(in crate::ui::features::shelf) fn record_width(viewport: f32) -> f32 {
    (viewport - 2.0 * super::grid::GUTTER * 2.0).min(overlay::RECORD_MAX_WIDTH)
}

/// 这张卡此刻该多高：贴着内容长，封顶才开始滚。
///
/// 高是**报上来**的而不是画完再量的（`overlay::centered` 写清了这一手换来什么）：报的数与
/// 真实高对不上时卡会偏上或偏下几像素，不截断、不挡别的东西。行数在这儿是已知的，所以
/// 不必像扫描那一屏那样先给一个定死的框 —— 一本书的卡就该是一本书的高。
fn record_height(rows: usize, running: bool) -> f32 {
    let foot = if running { FOOT_HEIGHT } else { 0.0 };
    // 三层之间那两条分隔线（标题层 / 内容层 / 动作层）各占 1 像素，报的高要带上；
    // 一行都还没有时也要给那格空态留位：在跑等的是那条进度条，没在跑等的是
    // 「暂无导入记录」那句话 —— 两态都至少占一行的高。
    let body = rows.max(1) as f32 * ROW_HEIGHT;
    (HEAD_HEIGHT + body + foot + 2.0).min(overlay::RECORD_MAX_HEIGHT)
}

/// 一本书在这一批记录里的样子。
#[derive(Debug, Clone)]
pub(in crate::ui::features::shelf) struct Record {
    /// core 报字节时用的那个名字（候选的文件名）。行与行靠它对上，不拿它当屏幕上的话。
    key: SharedString,
    /// 屏幕上说的那个名字：去掉那一段扩展名。
    name: SharedString,
    /// 扩展名的大写（`EPUB`）。取不到就没有那一格，不编。
    format: Option<SharedString>,
    /// 字节数：在跑那一档是进度条的分母，结清那一档是这本书的大小。
    size: Option<u64>,
    /// 已经传到的字节。`None` = 这本还没开始传（或压根不报字节）。
    done: Option<u64>,
    /// 结清之后的结论。`None` = 还在路上。
    landed: Option<Landed>,
    /// 跑完这一格花了多久（秒）。一个包算一趟，包里那几条是同一个数（见 `ImportOutcome`）。
    secs: Option<f32>,
    /// 落定那一刻（Unix 秒）。在跑那一档它是「这一批开始的时间」，还不作数；结清与从库里
    /// 读回来的那些才拿它说日期。
    made_at: i64,
}

/// 结清那一档的三种结局。
///
/// 「当时怎么样了」与「现在还能不能打开」是两件事：书后来被读者删了，那次导入仍然发生过，
/// 记录要留着，只是那颗「打开」没有对象了 —— 所以 `book_id` 挂在结局上，而不是结局挂在
/// `book_id` 上。
#[derive(Debug, Clone, Copy)]
enum Landed {
    /// 新落地的一本。`book_id` = 现在架上还有那一本（读回来时由那条 LEFT JOIN 现算）。
    Added { book_id: Option<i64> },
    /// 内容哈希已经有那一行了：架上那一本早就在。
    Duplicate { book_id: Option<i64> },
    /// 没进来，按类别。屏幕上那句话在 [`reject_label`] 里现取。
    Rejected(ImportReject),
}

/// 没进来那一档屏幕上怎么说。
///
/// **穷尽的 `match`**：加一类 `ImportReject` 而忘了在这儿加一句，编译就过不去。这条不变量
/// 从前靠 core 那句「五格与五键一一对应」的注释加一族测试守着，搬到界面这一层之后它成了
/// 编译期的事。键的字面量写在各臂里（而不是交一个词干出去再拼），是因为 `l10n` 那扇扫源码
/// 的门只认带字面量的那几种调用 —— 门要看得见，键打错才拦得住。
fn reject_label(kind: ImportReject, cx: &App) -> SharedString {
    match kind {
        ImportReject::NeedsPassword => tr(cx, "shelf.status_needs_password"),
        ImportReject::Encrypted => tr(cx, "shelf.status_encrypted"),
        ImportReject::CorruptContainer => tr(cx, "shelf.status_corrupt"),
        ImportReject::UnsupportedFormat => tr(cx, "shelf.status_unsupported"),
        ImportReject::Unreadable => tr(cx, "shelf.status_unreadable"),
    }
}

/// 文件名拆成「屏幕上那个名字」与「那一段扩展名」。
///
/// 认作扩展名的条件：点号前有名、点后是 1..=6 个 ASCII 字母数字。这样 `.envrc`（点在开头）、
/// `a.`、`archive.tar.gz.bak` 这类都不会被削掉一段假扩展名。
///
/// 扫描那一屏共用这一条（`scan` 的行也是「名字 + 类型」）：削法只有一处，改日改它两头一起改。
pub(in crate::ui::features::shelf) fn split_display_name(file_name: &str) -> (&str, Option<&str>) {
    match file_name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=6).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (stem, Some(ext))
        }
        _ => (file_name, None),
    }
}

/// 屏幕上那一行 → 库里那一行。
///
/// 存的是 `key`（当时那个**完整**文件名，带扩展名），不是削过的那份：削法只有一处
/// （[`split_display_name`]），读回来再削一遍，改日改那一条规则就两头一起改。
/// 还在路上（没有结局）的那些不写库 —— 库里那一行说的是一件**发生过**的事。
fn log_entry(row: &Record) -> Option<folio_core::ImportLogEntry> {
    let (landing, book_id) = match row.landed? {
        Landed::Added { book_id } => (ImportLanding::Added, book_id),
        Landed::Duplicate { book_id } => (ImportLanding::Duplicate, book_id),
        // 没进来的那几本没有 `book_id` 可交：那一颗「打开」本来就不摆。
        Landed::Rejected(kind) => (ImportLanding::Rejected(kind), None),
    };
    Some(folio_core::ImportLogEntry {
        made_at: row.made_at,
        name: row.key.to_string(),
        format: row.format.as_ref().map(|format| format.to_string()),
        size: row.size,
        landing,
        book_id,
        secs: row.secs.unwrap_or(0.0),
    })
}

/// 库里那一行 → 屏幕上那一行。
///
/// `book_id` 是读库那条 LEFT JOIN 现算的：那一本被删掉了它就是 `None`，而结局那一格照原样
/// 说「成功」—— 记录说的是当时发生过的事，不是现在的牌面。
fn record_from_log(entry: &folio_core::ImportLogEntry) -> Record {
    let (stem, ext) = split_display_name(&entry.name);
    let landed = match entry.landing {
        ImportLanding::Added => Landed::Added {
            book_id: entry.book_id,
        },
        ImportLanding::Duplicate => Landed::Duplicate {
            book_id: entry.book_id,
        },
        ImportLanding::Rejected(kind) => Landed::Rejected(kind),
    };
    Record {
        key: entry.name.as_str().into(),
        name: stem.into(),
        // 库里那一格存的是当时削出来的大写扩展名；老行没有的那儿退回从文件名再削一次，
        // 削法是同一条规则（[`split_display_name`]），不会削出第二种形状。
        format: entry
            .format
            .clone()
            .map(|format| format.into())
            .or_else(|| ext.map(|ext| ext.to_uppercase().into())),
        size: entry.size,
        done: None,
        landed: Some(landed),
        secs: Some(entry.secs),
        made_at: entry.made_at,
    }
}

impl ShelfView {
    /// 提交一批导入：在跑那一列重开，「读者关掉过这张卡」也一并清掉。
    ///
    /// 不抹掉上一批的行，新批的第一条字节读数就会落在旧行上 —— 屏幕上两批混成一列。
    pub(in crate::ui::features::shelf) fn begin_import_records(&mut self, cx: &mut Context<Self>) {
        self.import_live.clear();
        // 新的一批自己会把卡抬起来（`importing` 那一格）；上一批「已结清」那一档与
        // 菜单翻开历史的那一档就此作废。
        self.import_settled = false;
        self.import_records_open = false;
        self.import_status_dismissed = false;
        cx.notify();
    }

    /// 一条字节读数 → 那一行的进度。名字对不上就新开一行：那是这本书第一次露面。
    pub(in crate::ui::features::shelf) fn note_import_bytes(
        &mut self,
        name: &str,
        done: u64,
        total: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let key: SharedString = name.into();
        if let Some(row) = self.import_live.iter_mut().find(|row| row.key == key) {
            row.done = Some(done);
            // 分母只在报得上来的时候更新：`None`（服务器没给 `Content-Length` 那种）不该
            // 把已经量到的那个总数抹成「不知道」。
            row.size = total.or(row.size);
        } else {
            let (stem, ext) = split_display_name(name);
            self.import_live.push(Record {
                key,
                name: stem.into(),
                format: ext.map(|ext| ext.to_uppercase().into()),
                size: total,
                done: Some(done),
                landed: None,
                secs: None,
                made_at: folio_core::infra::time::now(),
            });
        }
        cx.notify();
    }

    /// 一批结清：逐本从结果表长成行，接到历史那一头（新的在前），在跑那一列就此清空。
    ///
    /// 这里**不**跟在跑的那几行逐条对上：一个包会抽出多条（条目名各自一行），外面那个包名
    /// 不该留在列里替里面那几十本说话。
    pub(in crate::ui::features::shelf) fn finish_import_records(
        &mut self,
        outcomes: &[ImportOutcome],
        cx: &mut Context<Self>,
    ) {
        // 一批盖一个时刻：库里那几行与屏幕上这一列说的是同一批，各盖各的钟会让「今天」
        // 那一格把同一批拆到两天去（跨过日界线那一趟尤其明显）。
        let made_at = folio_core::infra::time::now();
        let mut rows: Vec<Record> = outcomes
            .iter()
            .map(|outcome| {
                let file_name = outcome.candidate.name.as_str();
                let (stem, ext) = split_display_name(file_name);
                let landed = match &outcome.result {
                    Ok(Imported::Added(registered)) => Landed::Added {
                        book_id: Some(registered.book_id),
                    },
                    Ok(Imported::Duplicate { book_id }) => Landed::Duplicate {
                        book_id: Some(*book_id),
                    },
                    // 认不出类别的也算「读不动」：与 core 那张报表同一个口径
                    // （`ImportReport::tally`），屏幕上不该冒出第二句话。
                    Err(err) => {
                        Landed::Rejected(err.import_reject().unwrap_or(ImportReject::Unreadable))
                    }
                };
                Record {
                    key: file_name.into(),
                    name: stem.into(),
                    format: ext.map(|ext| ext.to_uppercase().into()),
                    size: outcome.candidate.size,
                    done: None,
                    landed: Some(landed),
                    secs: Some(outcome.duration.as_secs_f32()),
                    made_at,
                }
            })
            .collect();
        // 落库：重启之后这张卡还有话说。库没装上（还在启动那一段）就只留在内存里 ——
        // 少写一次盘不该让这一次导入看起来像失败了，而那一本的书库行是真的。
        if let Some(store) = services::app_store::handle(cx) {
            let entries: Vec<folio_core::ImportLogEntry> =
                rows.iter().filter_map(log_entry).collect();
            if let Err(err) = store.record_imports(&entries) {
                tracing::warn!("ShelfView: 导入记录没写进库：{err:#}");
            }
        }
        // 结清的先后：读者先看见的应该是**先结清**的那几本，所以这一列倒过来再插到历史头上
        // （历史整体是新的在前）。
        rows.reverse();
        self.import_history.splice(0..0, rows);
        // 与库里那个封顶同一个数：库只留这么多，界面再留一个更小的就是两处各定一遍规矩。
        self.import_history.truncate(LOG_LIMIT as usize);
        self.import_live.clear();
        // 这一批结清了：逐本着落值得再交代一次（卡的自动来源之一）。
        self.import_settled = true;
        // 上一批被关掉过不该压住这一批的交代：有新东西要说就重新开口。
        self.import_status_dismissed = false;
        cx.notify();
    }

    /// 架面起起来那一次，把库里的记录读回这一列（只读一次，读到了才算数）。
    ///
    /// 判据是「库装上了没有」而不是「第几帧」：库还没装上时这一手什么都不做，下一帧再来一遍，
    /// 于是慢机器与快机器上看到的是同一个结果。
    pub(in crate::ui::features::shelf) fn load_import_log(&mut self, cx: &mut Context<Self>) {
        let Some(store) = services::app_store::handle(cx) else {
            return;
        };
        self.import_log_loaded = true;
        match store.import_log(LOG_LIMIT) {
            Ok(entries) => {
                // 读回来的那些排在**已有历史之后**：这一次运行里刚跑完的那一批更新，而库里
                // 那份是重启前留下的 —— 两边都是新的在前，接起来就是连续的。
                let loaded = entries.iter().map(record_from_log).collect::<Vec<_>>();
                self.import_history.extend(loaded);
                self.import_history.truncate(LOG_LIMIT as usize);
            }
            Err(err) => tracing::warn!("ShelfView: 导入记录没读回来：{err:#}"),
        }
    }

    /// 清空那份历史：内存与库里一起清，**书库一条不动** —— 那几本书都还在架上。
    ///
    /// 只清内存不算清干净：下一次架面起起来又会从库里读回来，读者看到的是「按了一下，
    /// 过一会儿又冒出来」。库没装上时（还在启动那一段）就只清内存那一半。
    ///
    /// 卡**不收**：清完摆出空态（「暂无导入记录」，见 [`Self::record_body`]），读者当场
    /// 看得见动作生效了。那条给「在跑」准备的进度条只认 `running`（同处有注），空态不会
    /// 再把它摆出来。
    fn clear_import_records(&mut self, cx: &mut Context<Self>) {
        self.import_history.clear();
        if let Some(store) = services::app_store::handle(cx)
            && let Err(err) = store.clear_import_log()
        {
            tracing::warn!("ShelfView: 库里的导入记录没清掉：{err:#}");
        }
        cx.notify();
    }

    /// 关掉这张卡：这一批不再说第二遍；提交下一批、或一批跑完，都重新开口。
    fn dismiss_import_status(&mut self, cx: &mut Context<Self>) {
        self.import_status_dismissed = true;
        // 关掉的是「别再说了」：这一批的交代与菜单翻开的那一档一起作废。
        self.import_settled = false;
        self.import_records_open = false;
        cx.notify();
    }

    /// 回归门用：按读者关掉这张卡的那一手。真实的那一路是点卡外面或按返回，门要命中的
    /// 是弹窗层外面的坐标 —— 够不着，所以门走这扇门，动作与回调里那句是同一个。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn dismiss_import_status_for_test(&mut self, cx: &mut Context<Self>) {
        self.dismiss_import_status(cx);
    }

    /// 记录那一张卡；**没话可说时是 `None`**，这一格也就不占屏幕上任何位置。
    ///
    /// 容器与建组、链接导入、扫描结果那几张同一个（[`overlay::centered`]）。宽与高都按视口
    /// 与行数现算（见 [`record_width`] 与 [`record_height`]）：写死 420 在手机上会把右边那
    /// 两颗钮顶到屏幕外，而这张卡最主要的动作就是那两颗。
    ///
    /// `viewport` 是内容区那一段的宽（由 `render` 从窗口报上来，这一层不认识窗口）。
    pub(in crate::ui::features::shelf) fn import_status_popover(
        &self,
        viewport: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.import_status_dismissed || !self.import_status_visible() {
            return None;
        }
        let shelf = cx.entity().clone();
        let running = self.importing.is_some();
        let rows = self.record_rows(running).len();
        Some(overlay::centered(
            "shelf-import-status-popover",
            record_height(rows, running),
            &shelf,
            |this, cx| this.dismiss_import_status(cx),
            self.record_body(record_width(viewport), cx),
        ))
    }

    /// 这张卡此刻该不该**自己冒出来**：这一批在跑、刚跑完有话要说，或者读者从菜单里
    /// 点名要看。
    ///
    /// 历史非空**不算**：库里那份是重启之后读回来的，只凭它就把卡抬起来 = 每次开软件
    /// 先弹一张昨天的记录。读者要看历史，走三点菜单那一颗。
    fn import_status_visible(&self) -> bool {
        self.importing.is_some()
            || !self.import_live.is_empty()
            || self.import_settled
            || self.import_records_open
    }

    /// 三点菜单里那一颗「导入记录」该不该可按：在跑、在跑那一列有行，或者历史里还留着
    /// 哪几本的事。没话可说时那颗该是**按不动**的，而不是点开一张空卡（见 `gpui-kit`
    /// 设计规约：不合适的动作要么藏掉，要么禁用并给出理由）。
    ///
    /// 与 [`Self::import_status_visible`] 差的就是「历史非空」这一条：历史只让**菜单**
    /// 可按，不让卡自己跳出来。
    pub(in crate::ui::features::shelf) fn import_records_wanted(&self) -> bool {
        self.importing.is_some() || !self.import_live.is_empty() || !self.import_history.is_empty()
    }

    /// 从三点菜单翻开导入记录。
    ///
    /// 读者主动要看的，之前「把这张卡关掉过」那个意图就此作废 —— 那一手挡的是**自动**冒出来
    /// 的那几次，不挡这一次。历史是空的时候菜单那颗按不动，所以这儿不必再判一遍。
    pub(in crate::ui::features::shelf) fn open_import_records(&mut self, cx: &mut Context<Self>) {
        self.import_records_open = true;
        self.import_status_dismissed = false;
        cx.notify();
    }

    /// 回归门用：菜单里那颗「导入记录」的那一手（门在外壳那一层，够不到 shelf 内部的可见性）。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn open_import_records_for_test(&mut self, cx: &mut Context<Self>) {
        self.open_import_records(cx);
    }

    /// 卡的内容：头顶一行标题，中间那条清单，脚下一排动作。
    ///
    /// 三层之间各垫一条 [`Separator::horizontal`]：标题与清单、清单与动作是两个层级，
    /// 各归各的那条横线替它们划界 —— 没有那两条线，滚动着的清单行与顶上那行标题、
    /// 底下那颗按钮会糊成一片同质的内容。
    fn record_body(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let running = self.importing.is_some();
        let rows = self.record_rows(running);
        let count = rows.len();
        let height = record_height(count, running);
        let head = self.record_head(running, cx);
        let foot = self.record_foot(running, cx);
        let list_entity = cx.entity().clone();
        let sizes = Rc::new(
            (0..count)
                .map(|_| size(px(10000.), px(ROW_HEIGHT)))
                .collect::<Vec<_>>(),
        );
        let scroll = &self.record_scroll;
        v_flex()
            .id("shelf-import-records")
            // 外壳那扇回归门按 id 认这张卡画没画出来（`ui::shell::tests`）。id 沿用从前
            // 那一行的：门断言的是「在导入时这句话在不在屏幕上」。
            .test_support()
            .w(px(width))
            .h(px(height))
            .flex_col()
            .child(head)
            // 标题层与内容层之间那条线。
            .child(Separator::horizontal())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(count > 0, |list| {
                        list.child(
                            gpui_kit::base::v_virtual_list(
                                list_entity,
                                "shelf-import-record-rows",
                                sizes,
                                move |this, range, _window, cx| this.record_cells(range, cx),
                            )
                            // 句柄跨帧握着：不顶这一句，清单每帧把偏移抹回 0，手机上滑不动。
                            .track_scroll(scroll)
                            .flex_1(),
                        )
                    })
                    // 空的那一格按状态分叉：**在跑**还没有一行可列（刚提交、第一条字节读数
                    // 还没上来）摆那条不定进度的条子；**没在跑**还空着（历史被清空、或本来
                    // 就没有）说的是「暂无导入记录」—— 这里从前不分状态，清完记录之后那条
                    // 给「在跑」准备的进度条就永远转下去（真机上读者报的那个「删完一直加载」）。
                    .when(running && count == 0, |empty| {
                        empty
                            .px(px(super::grid::GUTTER))
                            .child(Progress::new("shelf-record-waiting").w_full().loading(true))
                    })
                    .when(!running && count == 0, |empty| {
                        empty
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(muted_label(tr(cx, "shelf.record_empty"), cx))
                    }),
            )
            // 内容层与动作层之间那条线：foot 只在跑的那一档存在，线跟着它走。
            .when(running, |column| column.child(Separator::horizontal()))
            .child(foot)
            .into_any_element()
    }

    /// 这一帧列的是哪一列：在跑的是这一批，否则是历史。
    fn record_rows(&self, running: bool) -> &[Record] {
        if running {
            &self.import_live
        } else {
            &self.import_history
        }
    }

    /// 清单里那几格。
    fn record_cells(
        &self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let running = self.importing.is_some();
        let rows = self.record_rows(running);
        range
            .filter_map(|ix| rows.get(ix).map(|row| (ix, row)))
            .map(|(ix, row)| self.record_cell(ix, row, cx))
            .collect()
    }

    /// 头顶那一行：**只有标题**，右上两颗动作。
    ///
    /// 成功几本 / 没进来几本、在跑那一档的「已完成 / 共」都不在这儿摆：逐行自己带
    /// 状态与进度条，卡头再汇总一遍只是让读者把同一批账数两次。标题因此是这一屏
    /// 唯一的大字，层级由它定；次要的数字一律下沉到行里。
    fn record_head(&self, running: bool, cx: &mut Context<Self>) -> AnyElement {
        let title: SharedString = if running {
            tr(cx, "shelf.status_importing")
        } else {
            tr(cx, "shelf.record_title")
        };
        let clear_entity = cx.entity().clone();
        let close_entity = cx.entity().clone();
        h_flex()
            .id("shelf-record-head")
            .test_support()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(super::grid::GUTTER))
            .pt_3()
            .pb_2()
            .child(
                div()
                    .id("shelf-record-title")
                    .test_support()
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                // 右上角那两颗：垃圾桶只管这一列记录（一本书都不动），× 收掉这张卡。
                // 在跑那一档没有可清的东西，也不该让读者在批中间一键抹掉已经报上来的行。
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .when(!running, |group| {
                        group.child(
                            Button::new("shelf-record-clear")
                                .ghost()
                                .small()
                                .icon(IconName::Trash)
                                .accessibility_label(tr(cx, "shelf.record_clear"))
                                .on_click(move |_, _, cx| {
                                    clear_entity
                                        .update(cx, |this, cx| this.clear_import_records(cx))
                                }),
                        )
                    })
                    .child(
                        Button::new("shelf-record-close")
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .accessibility_label(tr(cx, "shelf.scan_close"))
                            .on_click(move |_, _, cx| {
                                close_entity.update(cx, |this, cx| this.dismiss_import_status(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 脚下那一排：在跑给一颗「取消导入」；结清不给动作 —— 事已经过去，那几行自己会说话。
    fn record_foot(&self, running: bool, cx: &mut Context<Self>) -> AnyElement {
        if !running {
            return div().into_any_element();
        }
        let entity = cx.entity().clone();
        let cancel_label = tr(cx, "shelf.cancel_import");
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px(px(super::grid::GUTTER))
            .pb_2()
            .child(
                Button::new("shelf-import-cancel")
                    .ghost()
                    .small()
                    .label(cancel_label.clone().to_string())
                    .accessibility_label(cancel_label)
                    .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.cancel_import(cx))),
            )
            .into_any_element()
    }

    /// 一格：书名是唯一焦点，右边一枚状态标签与「打开」，下面一行次要读数。
    ///
    /// 「打开」只在有书可开时摆：没进来的那几本没有对象可作用，一颗按不动的钮
    /// 比不摆更糟。元素 id 带上行号 —— 一列里同一个动作会出现很多次，共用一个 id 会让
    /// gpui 把相邻几行的状态串在一起。
    fn record_cell(&self, ix: usize, row: &Record, cx: &mut Context<Self>) -> AnyElement {
        let meta = self.record_meta(ix, row, cx);
        let status = self.record_status(ix, row, cx);
        let opener = match row.landed {
            Some(Landed::Added {
                book_id: Some(book_id),
            })
            | Some(Landed::Duplicate {
                book_id: Some(book_id),
            }) => {
                let entity = cx.entity().clone();
                let label = tr(cx, "shelf.record_open");
                Some(
                    Button::new(format!("shelf-record-open-{ix}"))
                        .ghost()
                        .small()
                        .label(label.clone().to_string())
                        .accessibility_label(label)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |_, cx| cx.emit(ShelfEvent::Open(book_id)))
                        }),
                )
            }
            _ => None,
        };
        v_flex()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap_1()
            .px(px(super::grid::GUTTER))
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_weight(FontWeight::MEDIUM)
                            .child(row.name.clone()),
                    )
                    .children(status)
                    .children(opener),
            )
            .child(meta)
            .into_any_element()
    }

    /// 结清那一档的状态：**只有要读者处理的那几类**才出一枚语义色标签。
    ///
    /// 成功与「已在架上」不在这儿出标签：它们在名字右边排成一列中性胶囊，把书名的
    /// 层级压下去（设计指南：强调是有限预算），而这两件事本来也不需要读者做什么 ——
    /// 行尾那颗「打开」已经说明这本书进来了。这两句改走 [`Self::record_meta`] 那一行
    /// 的**文字**（`成功 · 11-04 · EPUB`），信息不丢、视觉压力降一档。
    ///
    /// 真正要读者处理的（没进来 / 要口令 / 带加密 …）仍是一枚语义色标签：那一档的
    /// 读者需要**一眼看见**，压进小字反而藏起来了。文字本身就是读屏读到的那一句，
    /// 不再额外挂一份 aria 名。
    fn record_status(&self, ix: usize, row: &Record, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = match row.landed? {
            // 成功 / 已在架上不占标签位：它们进 `record_meta` 那一行（见本函数文档）。
            Landed::Added { .. } | Landed::Duplicate { .. } => return None,
            Landed::Rejected(kind) => reject_label(kind, cx),
        };
        let fg = cx.theme().danger;
        Some(
            div()
                .id(format!("shelf-record-status-{ix}"))
                .test_support()
                .flex_shrink_0()
                .px(px(8.))
                .py(px(2.))
                .rounded_full()
                .bg(cx.theme().muted)
                .text_size(px(11.))
                .text_color(fg)
                .child(label)
                .into_any_element(),
        )
    }

    /// 名字下面那一行：进度条（还在跑）或一串次要读数（已结清）。
    ///
    /// 要读者处理的那几档（没进来 / 要口令）状态在名字右边那枚语义色标签上；**成功 /
    /// 已在架上**没有标签，这两句落成这一行的**第一个词**（`成功 · 11-04 · EPUB`）——
    /// 信息不丢，而整张卡的视觉焦点仍是书名（见 [`Self::record_status`]）。
    fn record_meta(&self, ix: usize, row: &Record, cx: &mut Context<Self>) -> AnyElement {
        if row.landed.is_none() {
            // 还在路上：条子按字节走。总数不知道（`None`）就是不定进度那一档。
            let fraction = match (row.done, row.size) {
                (Some(done), Some(total)) if total > 0 => {
                    Some((done as f32 * 100.0 / total as f32).clamp(0.0, 100.0))
                }
                _ => None,
            };
            return div()
                .w_full()
                .child(
                    Progress::new(format!("shelf-record-bar-{ix}"))
                        .w_full()
                        .loading(fraction.is_none())
                        .value(fraction.unwrap_or(0.0)),
                )
                .into_any_element();
        };
        // 「成功 · 11-04 · EPUB · 666.63 MB · 9 秒」：状态词打头（成功 / 已在架上），
        // 之后是说得出着落的那些格子；今天那一批不带日期（见 `record_date`）。
        let status = match row.landed {
            Some(Landed::Added { .. }) => Some(tr(cx, "shelf.status_added")),
            Some(Landed::Duplicate { .. }) => Some(tr(cx, "shelf.status_duplicate")),
            // 没进来的那几档不吃这一行：它们在名字右边那枚语义色标签上，句子更长
            // （`文件已损坏` 那几句），塞进这一行会把后面几格挤没。
            Some(Landed::Rejected(_)) | None => None,
        };
        let today =
            folio_core::local_day(folio_core::infra::time::now(), host::local_offset_secs());
        let mut parts: Vec<String> = Vec::new();
        if let Some(status) = status {
            parts.push(status.to_string());
        }
        if let Some(day) = record_date(row.made_at, today) {
            parts.push(day.to_string());
        }
        if let Some(format) = row.format.as_ref() {
            parts.push(format.to_string());
        }
        if let Some(size) = row.size {
            parts.push(fmt_size(size));
        }
        if let Some(secs) = row.secs {
            // 说「9 秒」而不是「8.8s」：时长那几句是这套界面共用的口径（统计、笔记同一份），
            // 不为这一张卡另立一种写法。
            parts.push(duration(cx, secs.round() as i64).to_string());
        }
        muted_label(parts.join(" · "), cx).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    //! 这两条是从真机上那个错来的：卡的宽写死 420，比手机那一档 384 逻辑像素的视口还宽，
    //! 于是卡顶成满幅、右边那两颗钮（关闭与清空）整个掉到屏幕外 —— 读者看到的正是「这张
    //! 卡关不掉」。高也写死 420，一本书的卡占满整屏。
    //!
    //! 像素在无头窗口里量不到，能钉的是算尺寸的那两个函数：**宽不许吃到视口的边**，
    //! **高贴着行数走、只有清单长到封顶才占满那 420**。

    use crate::ui::features::shelf::overlay;

    use super::{record_height, record_width};

    /// 手机上（384 那一档）宽由视口说了算，两边各留一档留白；桌面上才是那个 420 的上限。
    #[test]
    fn the_card_never_eats_into_the_viewport() {
        // 手机那一档：留出的是左右各 24，两颗钮因此在屏内。
        assert_eq!(record_width(384.0), 336.0);
        assert!(
            record_width(384.0) < 384.0 - 2.0 * super::super::grid::GUTTER,
            "宽不许贴着视口的边，那等于把动作钮推到屏幕外"
        );
        // 再宽就该收在上限，而不是跟着窗口长。
        assert_eq!(record_width(1600.0), overlay::RECORD_MAX_WIDTH);
    }

    /// 高跟着行数走：一本书就是一本书的高，清单长到封顶才开始滚。
    #[test]
    fn the_card_hugs_its_rows_until_the_cap() {
        let one = record_height(1, false);
        let two = record_height(2, false);
        assert!(one < overlay::RECORD_MAX_HEIGHT, "一本书的卡不该占满那 420");
        assert!(
            (two - one - super::ROW_HEIGHT).abs() < 0.01,
            "多一行就该多一行的高"
        );
        // 封顶之后不再长：那一段交给清单自己滚。
        assert_eq!(record_height(500, true), overlay::RECORD_MAX_HEIGHT);
        // 刚提交、一行都还没有：给那条等着的条子留一格，不是零高。
        assert!(record_height(0, true) > record_height(0, false));
    }

    /// 卡头只剩一行标题：报上来的高得跟着收紧，否则 `overlay::centered` 把卡摆偏
    /// （[`record_height`] 那条注释）。旧的两行（标题 + 汇总）是 68。
    #[test]
    fn the_head_reports_only_one_line() {
        // 常量折叠会把 `HEAD_HEIGHT < 60.0` 变成「断言的真值是常量」—— 那正是这一条
        // 要的：改了那个数就让这条红，而不是让 clippy 去猜。写成运行时比较是为了让
        // 失败信息里带上当前的数（常量断言的失败信息是最短那一句）。
        let head = std::hint::black_box(super::HEAD_HEIGHT);
        assert!(head < 60.0, "卡头只剩一行标题，不该还按两行给高：{head}");
    }
}
