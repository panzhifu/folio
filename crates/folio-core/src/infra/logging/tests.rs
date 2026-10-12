    use super::{Directive, LogFile, build, log_file, subscriber};
    use crate::infra::config::Logging;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::Registry;
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter as _;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

    /// 一支 target 的五档级别各发一条。
    ///
    /// 只能写成宏：`tracing` 的 `target:` 要求**字面量**，运行时换个字符串换不掉
    /// 编译期定下的 Metadata。
    macro_rules! probe {
        ($target:literal) => {
            tracing::error!(target: $target, "error");
            tracing::warn!(target: $target, "warn");
            tracing::info!(target: $target, "info");
            tracing::debug!(target: $target, "debug");
            tracing::trace!(target: $target, "trace");
        };
    }

    /// 四支探针：自己的两支、界面的一支、依赖库的一支。
    macro_rules! probe_all {
        () => {
            || {
                probe!("folio_core::layout");
                probe!("folio_core::store");
                probe!("folio_app::ui::shelf");
                probe!("wgpu_core");
            }
        };
    }

    /// 把过滤器真的装成 subscriber、发一遍 `probes`、返回真的落了笔的那些 `目标@级别`。
    ///
    /// 问的是「装好之后到底有没有那一行」，而不是 `Targets` 的判断结果 —— 级别过滤真正的
    /// 形状是宏 → callsite → Interest → 层，中间任何一环没接上，屏幕上就是没有。
    ///
    /// `probes` 必须由调用方用 [`probe_all!`] 现场展开，不能在这个函数里发：callsite 是
    /// 编译期静态的，而它的 Interest 会被**缓存**，共用一份就等于第一个跑的测试替后面
    /// 所有测试定下结论。用 `set_default`（线程局部），所以并发用例互不影响，
    /// 也完全不碰进程的环境变量。
    fn captured(logging: &Logging, env: Option<&str>, probes: impl FnOnce()) -> Vec<String> {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default()
            .with(build(logging, env))
            .with(Capture(seen.clone()));
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            probes();
        }
        std::mem::take(
            &mut *seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// 只记账、不输出的那层。
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _cx: Context<'_, S>) {
            let meta = event.metadata();
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("{}@{}", meta.target(), meta.level().as_str()));
        }
    }

    /// `level` 只管 folio 自己那一支，依赖库固定在 warn：这是「打开 debug」不会把整个
    /// GPU 栈一起打开的那道保障。
    #[test]
    fn the_config_level_covers_folio_only() {
        let logging = Logging {
            level: "trace".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        for mine in [
            "folio_core::layout",
            "folio_core::store",
            "folio_app::ui::shelf",
        ] {
            assert!(
                seen.contains(&format!("{mine}@TRACE")),
                "{mine} 该被打开：{seen:?}"
            );
        }
        let deps: Vec<&String> = seen
            .iter()
            .filter(|n| n.starts_with("wgpu_core@"))
            .collect();
        assert_eq!(
            deps,
            vec!["wgpu_core@ERROR", "wgpu_core@WARN"],
            "依赖库只留 warn 以上"
        );
    }

    /// 同一条前缀上，环境变量压过配置文件。
    #[test]
    fn an_env_directive_beats_the_configured_one() {
        let logging = Logging {
            level: "warn".into(),
            targets: vec!["folio_core::layout=info".into()],
            ..Default::default()
        };
        let by_config = captured(&logging, None, probe_all!());
        assert!(by_config.contains(&"folio_core::layout@INFO".to_string()));
        assert!(!by_config.contains(&"folio_core::layout@DEBUG".to_string()));

        let by_env = captured(&logging, Some("folio_core::layout=debug"), probe_all!());
        assert!(
            by_env.contains(&"folio_core::layout@DEBUG".to_string()),
            "{by_env:?}"
        );
    }

    /// `FOLIO_LOG=debug` 按 `RUST_LOG` 的意思解释：一切 debug，**包括依赖库**。
    /// 这是「配置里那一档只管自己」的另一面 —— 想越过去看别人，得明说。
    #[test]
    fn a_bare_env_level_reaches_the_dependencies_too() {
        let seen = captured(&Logging::default(), Some("debug"), probe_all!());
        assert!(seen.contains(&"wgpu_core@DEBUG".to_string()), "{seen:?}");
    }

    /// 点名的那一支打开，没点名的照旧。
    #[test]
    fn a_target_directive_opens_exactly_that_branch() {
        let logging = Logging {
            level: "warn".into(),
            targets: vec!["folio_core::store=debug".into()],
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(
            seen.contains(&"folio_core::store@DEBUG".to_string()),
            "{seen:?}"
        );
        assert!(!seen.contains(&"folio_core::layout@DEBUG".to_string()));
    }

    /// 一条写坏只带走它自己，同批其余照常生效。
    #[test]
    fn one_bad_piece_costs_only_itself() {
        let logging = Logging {
            level: "off".into(),
            targets: vec!["junk".into(), "folio_core::layout=trace".into()],
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(seen.contains(&"folio_core::layout@TRACE".to_string()));
        assert!(
            !seen.iter().any(|n| n.starts_with("folio_core::store@")),
            "{seen:?}"
        );
    }

    /// 级别那一格写坏（手滑的 `verbose`）回落 info，而不是整档关掉 —— 没有日志的
    /// 程序比日志少一档的程序难查得多。
    #[test]
    fn an_unreadable_level_falls_back_instead_of_going_silent() {
        let logging = Logging {
            level: "verbose".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(seen.contains(&"folio_core::store@INFO".to_string()));
        assert!(!seen.contains(&"folio_core::store@DEBUG".to_string()));
    }

    /// `"off"` 是一档合法取值：调试时想彻底安静，不该被迫去关 `file` / `stderr`。
    #[test]
    fn off_silences_folios_own_code() {
        let logging = Logging {
            level: "off".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(!seen.iter().any(|n| n.starts_with("folio_")), "{seen:?}");
    }

    #[test]
    fn the_four_writings_parse_and_the_rest_stays_visible() {
        assert_eq!(
            Directive::parse("trace"),
            Directive::Default(LevelFilter::TRACE)
        );
        assert_eq!(
            Directive::parse(" folio_core::layout = debug "),
            Directive::Target("folio_core::layout".into(), LevelFilter::DEBUG)
        );
        assert_eq!(
            Directive::parse("nonsense"),
            Directive::Junk("nonsense".into())
        );
        assert_eq!(
            Directive::parse("folio_core=maybe"),
            Directive::Junk("folio_core=maybe".into())
        );
        // 空的那一条不是「关掉」，是「没说」—— 回落常见的那一档。
        assert_eq!(Directive::parse(""), Directive::Default(LevelFilter::INFO));
    }

    #[test]
    fn the_log_file_follows_the_dir_field_but_defaults_under_the_data_root() {
        let logging = Logging {
            file: true,
            ..Default::default()
        };
        assert_eq!(
            log_file(&logging),
            Some(crate::infra::paths::log_dir().join("folio.log"))
        );
        let logging = Logging {
            dir: "/tmp/folio-logs".into(),
            ..logging
        };
        assert_eq!(
            log_file(&logging),
            Some(std::path::PathBuf::from("/tmp/folio-logs/folio.log"))
        );
        assert_eq!(
            log_file(&Logging {
                file: false,
                ..logging
            }),
            None,
            "关掉就没有文件这一层"
        );
    }

    /// 空串那一条走的是 `build` 里 `FALLBACK_LEVEL` 那个分支，与 `"verbose"` 同一档。
    #[test]
    fn an_empty_level_is_the_fallback_one() {
        let logging = Logging {
            level: "  ".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(
            seen.contains(&"folio_core::store@INFO".to_string()),
            "{seen:?}"
        );
    }

    /// 开机轮转：上一次运行那份让位给 `.1`，新的从空开始。
    #[test]
    fn the_previous_run_moves_aside_before_the_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        std::fs::write(&path, "上一轮").unwrap();

        LogFile::new(&path, 0, 3)
            .make_writer()
            .write_all("这一轮".as_bytes())
            .unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "这一轮");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("folio.log.1")).unwrap(),
            "上一轮"
        );
    }

    /// 历史是从老往新挪的：`keep = 2` 时最老那份被挤掉，而不是把 `.2` 盖住又丢掉。
    #[test]
    fn history_shifts_from_the_oldest_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        for run in 1..=4 {
            std::fs::write(&path, format!("第{run}轮")).unwrap();
            LogFile::new(&path, 0, 2)
                .make_writer()
                .write_all(b"x")
                .unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("folio.log.1")).unwrap(),
            "第4轮"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("folio.log.2")).unwrap(),
            "第3轮"
        );
        assert!(
            !std::fs::exists(dir.path().join("folio.log.3")).unwrap(),
            "只留 keep 份"
        );
    }

    /// `keep = 0` 是「这一份只属于本轮」：旧的直接截掉，也不产生 `.1`。
    #[test]
    fn keeping_no_history_overwrites_the_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        std::fs::write(&path, "上一轮").unwrap();
        LogFile::new(&path, 0, 0)
            .make_writer()
            .write_all("这一轮".as_bytes())
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "这一轮");
        assert!(!std::fs::exists(dir.path().join("folio.log.1")).unwrap());
    }

    /// 写满之后**停手**而不是继续长 —— 这是「一次失控的 debug 循环吃掉磁盘」的兜底，
    /// 而且要在文件里说一句为什么停了，不然读日志的人会以为程序没做事。
    #[test]
    fn a_run_stops_writing_at_its_own_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        let mut sink = LogFile::new(&path, 30, 1).make_writer();
        for n in 0..50 {
            sink.write_all(format!("第{n}行，够长的内容\n").as_bytes())
                .unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() < 130, "封顶没起作用：{} 字节", text.len());
        assert!(text.contains("停止写入"), "要留下为什么说停了：{text}");
    }

    /// 目录建不出来（只读路径）时只是没有文件日志，程序照跑：`write` 返回成功，
    /// 因为日志写不了不该让排版那条路径报错。
    #[test]
    fn an_unwritable_path_costs_the_file_and_nothing_else() {
        let path = std::path::Path::new("/proc/definitely-not-writable/folio.log");
        let mut sink = LogFile::new(path, 0, 1).make_writer();
        assert!(sink.write_all("照样返回成功".as_bytes()).is_ok());
        assert!(sink.write_all("第二次也不报错".as_bytes()).is_ok());
    }

    /// `fmt` 层每条事件都重新 `make_writer` 一份：状态必须是共享的，
    /// 否则每轮都会重新轮转一次、只留下最后一行。
    #[test]
    fn the_shared_state_survives_a_fresh_writer_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        let sink = LogFile::new(&path, 0, 1);
        for _ in 0..3 {
            sink.make_writer().write_all(b"ab").unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ababab");
    }

    /// `FOLIO_LOG=warn` 遇上配置里的 `level = debug` 仍然要安静下来 —— 写这条环境变量的人
    /// 要的就是「别再刷了」，所以**光一个级别**那一条连 folio 自己那一支一起改掉。
    /// 点名的 `前缀=级别` 仍然照旧只影响那一支。
    #[test]
    fn a_bare_env_level_quiets_folios_own_code_as_well() {
        let logging = Logging {
            level: "debug".into(),
            ..Default::default()
        };
        let seen = captured(&logging, Some("warn"), probe_all!());
        assert!(seen.contains(&"folio_core::store@WARN".to_string()));
        assert!(
            !seen
                .iter()
                .any(|n| n.starts_with("folio_core::store@DEBUG")),
            "warn 没压过配置的 debug：{seen:?}"
        );
    }

    /// 尾巴多打的一个逗号不该把刚设的档位换回回落值（空片段会被读成「没说」）。
    #[test]
    fn a_trailing_comma_costs_nothing() {
        let logging = Logging {
            level: "debug".into(),
            ..Default::default()
        };
        let seen = captured(&logging, Some("warn,"), probe_all!());
        assert!(
            !seen.iter().any(|n| n.starts_with("folio_app@DEBUG")),
            "{seen:?}"
        );
    }

    /// 过滤器必须**真的**管得住依赖库，而不是「配置里说了、装配时没听见」。
    ///
    /// 走 `subscriber()` 这个生产入口而不是手拼层，是因为这一份曾经把三层装进
    /// `Vec<Box<dyn Layer>>`：`Vec` 不把成员层的 `Interest::never` 往外报，于是
    /// `level = warn` 的下限管不住 zbus / calloop 的 TRACE，实机跑十几秒就刷掉半份日志文件。
    /// `Option` 与元组那两层转发得老实 —— 这条就是钉住这件事。
    #[test]
    fn the_built_subscriber_filters_the_dependency_noise() {
        let dir = tempfile::tempdir().unwrap();
        let logging = Logging {
            level: "warn".into(),
            dir: dir.path().to_string_lossy().into_owned(),
            stderr: false,
            ..Default::default()
        };
        {
            let _guard = tracing::subscriber::set_default(subscriber(&logging, None));
            tracing::trace!(target: "zz_dependency", "这行不该进文件");
            tracing::warn!(target: "zz_dependency", "这行该进");
        }
        let text = std::fs::read_to_string(dir.path().join("folio.log")).unwrap();
        assert!(
            !text.contains("这行不该进文件"),
            "warn 的下限没管住依赖的 TRACE：{text}"
        );
        assert!(text.contains("这行该进"), "该留的那行也没留下：{text}");
    }
