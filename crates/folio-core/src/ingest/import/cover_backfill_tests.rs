//! 封面补齐维护作业的行为：失败账本重试与分页扫描。
//!
//! 这两条钉的都是「一趟扫完」老实现会静默坏掉的地方，所以单列一个测试文件，
//! 免得 `tests.rs` 继续长。

use crate::infra::task::StageCtx;
use crate::store::Store;

/// 封面补齐不再「一趟定生死」：没补成的行进失败账本，退避到点才重试；把占路的文件
/// 挪开之后，下一趟就能补回来。
///
/// 写盘失败是拿「书库根下有个同名文件占住 `covers/`」制造的，比拿坏容器去碰解析器稳定：
/// 解析器容忍垃圾字节时，这一趟会静默变成「这本书本来就没封面」。
#[test]
fn cover_backfill_retries_a_failed_book_after_backoff() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    std::fs::create_dir_all(&lib).unwrap();
    let db = tmp.path().join("folio.db");
    let store = Store::open_at(&db).unwrap();

    // 一份真能解出封面的 FB2：封面在 `<binary>` 里。
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
    let b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(png)
    };
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\" \
                      xmlns:l=\"http://www.w3.org/1999/xlink\">\n\
         <description><title-info><book-title>有封面的书</book-title>\n\
         <coverpage><image l:href=\"#cover.png\"/></coverpage>\n\
         </title-info></description>\n\
         <body><section><p>正文</p></section></body>\n\
         <binary id=\"cover.png\" content-type=\"image/png\">{b64}</binary>\n\
         </FictionBook>"
    );
    let path = lib.join("有封面的书.fb2");
    std::fs::write(&path, xml).unwrap();
    let meta = crate::content::format::DocMeta {
        title: "有封面的书".into(),
        format: crate::content::format::Format::Fb2,
        size: 0,
        ..crate::content::format::DocMeta::default()
    };
    let id = store.add_book(&path, &meta).unwrap();
    assert_eq!(store.book_cover(id).unwrap(), None, "起点：库里没有封面");

    // 同名文件占住 `covers/`：建不了目录，这次补写必然失败。
    let blocker = lib.join("covers");
    std::fs::write(&blocker, b"blocked").unwrap();

    let first = super::backfill_covers(&store, &StageCtx::detached());
    assert_eq!(first.failed, 1, "写不进去要记成没补成：{first:?}");
    assert_eq!(first.restored, 0);
    assert_eq!(first.pending, 1, "没补成的行要进账本");
    assert_eq!(
        store.pref_number(super::COVER_BACKFILL_KEY).unwrap(),
        Some(super::COVER_BACKFILL_VERSION),
        "扫到底是扫到底，版本记号照写"
    );

    // 退避没到点：第二趟不重扫，也不重试。
    let second = super::backfill_covers(&store, &StageCtx::detached());
    assert_eq!(second.scanned, 0, "记号在，不再扫全库");
    assert_eq!(second.retried, 0, "账本没到点，不该重试");
    assert_eq!(second.pending, 1);

    // 把占路的文件挪开，并把账本的退避时刻拨回过去。
    std::fs::remove_file(&blocker).unwrap();
    store
        .set_pref_text(
            super::COVER_BACKFILL_LEDGER_KEY,
            &format!("[{{\"id\":{id},\"failures\":1,\"retry_at\":0}}]"),
        )
        .unwrap();

    let third = super::backfill_covers(&store, &StageCtx::detached());
    assert_eq!(third.retried, 1, "到点的账要重试：{third:?}");
    assert_eq!(third.restored, 1, "路通了就该补回来");
    assert_eq!(third.pending, 0, "补成了要销账");
    assert_eq!(
        store.book_cover(id).unwrap().as_deref(),
        Some(&png[..]),
        "封面字节要能从 covers/ 读回来"
    );
    assert_eq!(
        store
            .pref_text(super::COVER_BACKFILL_LEDGER_KEY)
            .unwrap()
            .as_deref(),
        Some("[]"),
        "账本空了也要落盘，下次开机不再重试"
    );
}

/// 分页扫把整库走完，扫完把断点归零：下一版升级从头来，而不是从上次的尾部接着扫。
#[test]
fn cover_backfill_walks_the_library_one_page_at_a_time() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    std::fs::create_dir_all(&lib).unwrap();
    let db = tmp.path().join("folio.db");
    let store = Store::open_at(&db).unwrap();

    // 33 本、每本路径都不在：过了两页（32 + 1），每本都该被看到且回 `Unavailable`。
    for i in 0..33 {
        let path = lib.join(format!("缺文件-{i}.fb2"));
        let meta = crate::content::format::DocMeta {
            title: format!("缺文件-{i}"),
            format: crate::content::format::Format::Fb2,
            size: 0,
            ..crate::content::format::DocMeta::default()
        };
        store.add_book(&path, &meta).unwrap();
    }

    let report = super::backfill_covers(&store, &StageCtx::detached());
    assert_eq!(report.scanned, 33, "分页也要把整库走完：{report:?}");
    assert_eq!(report.skipped, 33);
    assert_eq!(report.restored, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(
        store.pref_number(super::COVER_BACKFILL_KEY).unwrap(),
        Some(super::COVER_BACKFILL_VERSION)
    );
    assert_eq!(
        store.pref_number(super::COVER_BACKFILL_CURSOR_KEY).unwrap(),
        Some(0),
        "扫完把断点归零"
    );
}
