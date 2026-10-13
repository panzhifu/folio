//! 看一个 `folio.db` 到底把钱花在哪了：按表/索引列出占用，并打印空页数与版本。
//!
//! 打开时会顺带跑一遍迁移（`Store::open_at`），所以拿**旧库的副本**跑它就能看到
//! v8（正文压缩 + 摘掉全文索引）与 auto_vacuum 的效果。
//!
//! ```sh
//! cp ~/.local/share/folio/folio.db /tmp/probe.db
//! cargo run -p folio-core --example db-size /tmp/probe.db
//! ```

use std::path::Path;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: db-size <path/to/folio.db>");
        std::process::exit(2);
    };
    let before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    // 打开即迁移。之后 print 的是新形状。
    let _store = folio_core::Store::open_at(Path::new(&path)).expect("库应该能打开");

    let conn = rusqlite::Connection::open(&path).expect("裸连接");
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap();
    let pages: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    let free: i64 = conn
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))
        .unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let auto: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
        .unwrap();

    println!("库文件      {} 字节（打开前 {before}）", page_size * pages);
    println!("user_version {version}   auto_vacuum {auto}（0=off 1=full 2=incremental）");
    println!(
        "空页        {free} × {page_size} = {} 字节",
        free * page_size
    );
    println!();

    let mut stmt = conn
        .prepare(
            "SELECT name, SUM(pgsize) AS bytes FROM dbstat
             GROUP BY name ORDER BY bytes DESC",
        )
        .expect("dbstat");
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (name, bytes) in rows {
        println!("{bytes:>12}  {name}");
    }
    println!();

    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    println!("书目   {}", count("SELECT count(*) FROM books"));
    println!("章节   {}", count("SELECT count(*) FROM chapters"));
    println!(
        "位置   {} 字节（章表里存的是它在容器里的位置，不是正文）",
        count("SELECT sum(length(locator)) FROM chapters"),
    );
    println!(
        "封面   {} 字节",
        count("SELECT sum(length(cover)) FROM books")
    );
    for (at, n) in conn
        .prepare("SELECT locator, count(*) FROM chapters GROUP BY locator ORDER BY 2 DESC LIMIT 5")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    {
        let head: String = at.chars().take(60).collect();
        println!("       x{n}  {head}");
    }
}
