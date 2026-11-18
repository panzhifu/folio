//! UI 源码依赖检查。解析模块与路径，不展开宏或代替 rustc 名称解析。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Item, Meta, Token, UseTree};

use crate::Result;

type Aliases = BTreeMap<String, Vec<String>>;

pub fn scan(root: &Path) -> Result<Vec<String>> {
    let entry = root.join("crates/folio-app/src/ui/mod.rs");
    let mut scan = Scanner {
        root,
        findings: BTreeSet::new(),
        visited: BTreeSet::new(),
    };
    scan.file(&entry, vec!["crate".into(), "ui".into()])?;
    Ok(scan.findings.into_iter().collect())
}

/// 检查一份模块源码，供负向测试和独立工具使用。
#[cfg(test)]
pub fn inspect(path: &str, module: &[&str], source: &str) -> Result<Vec<String>> {
    let syntax = syn::parse_file(source)?;
    let mut findings = BTreeSet::new();
    let mut visitor = Dependencies {
        path,
        module: module.iter().map(|s| (*s).into()).collect(),
        file_module_depth: module.len(),
        aliases: Aliases::new(),
        findings: &mut findings,
        external: Vec::new(),
        unsupported: Vec::new(),
    };
    visitor.file(&syntax);
    if !visitor.unsupported.is_empty() {
        return Err(visitor.unsupported.join("\n").into());
    }
    Ok(findings.into_iter().collect())
}

struct Scanner<'a> {
    root: &'a Path,
    findings: BTreeSet<String>,
    visited: BTreeSet<PathBuf>,
}

impl Scanner<'_> {
    fn file(&mut self, file: &Path, module: Vec<String>) -> Result<()> {
        for ancestor in file.ancestors().take_while(|path| *path != self.root) {
            let metadata = std::fs::symlink_metadata(ancestor)?;
            let linked = metadata.file_type().is_symlink();
            #[cfg(windows)]
            let linked = {
                use std::os::windows::fs::MetadataExt as _;
                linked || metadata.file_attributes() & 0x400 != 0
            };
            if linked {
                return Err(format!("UI 扫描不跟随链接：{}", ancestor.display()).into());
            }
        }
        let absolute = file.canonicalize()?;
        let ui = self.root.join("crates/folio-app/src/ui").canonicalize()?;
        if !absolute.starts_with(&ui) {
            return Err(format!("UI 模块越过扫描范围：{}", file.display()).into());
        }
        if !self.visited.insert(absolute) {
            return Err(format!("UI 模块重复或循环加载：{}", file.display()).into());
        }
        let path = file
            .strip_prefix(self.root)?
            .to_string_lossy()
            .replace('\\', "/");
        let syntax = syn::parse_file(&std::fs::read_to_string(file)?)
            .map_err(|error| format!("{path}: {error}"))?;
        if module.len() == 2
            && !syntax.items.iter().any(|item| {
                matches!(item, Item::Mod(item) if !test_only(&item.attrs) && rank(&item.ident.to_string()).is_some())
            })
        {
            return Err("UI 入口没有任何生产层模块，拒绝空覆盖".into());
        }
        let mut visitor = Dependencies {
            path: &path,
            file_module_depth: module.len(),
            module,
            aliases: Aliases::new(),
            findings: &mut self.findings,
            external: Vec::new(),
            unsupported: Vec::new(),
        };
        visitor.file(&syntax);
        if !visitor.unsupported.is_empty() {
            return Err(visitor.unsupported.join("\n").into());
        }
        let base = if file.file_name().is_some_and(|name| name == "mod.rs") {
            file.parent().ok_or("模块没有父目录")?.to_path_buf()
        } else {
            file.with_extension("")
        };
        for child in visitor.external {
            let parent = child
                .inline
                .iter()
                .fold(base.clone(), |path, segment| path.join(segment));
            let child_file = match child.path {
                Some(explicit) if child.inline.is_empty() => {
                    file.parent().ok_or("模块没有父目录")?.join(explicit)
                }
                Some(explicit) => parent.join(explicit),
                None => {
                    let single = parent.join(format!("{}.rs", child.name));
                    let directory = parent.join(&child.name).join("mod.rs");
                    match (single.is_file(), directory.is_file()) {
                        (true, false) => single,
                        (false, true) => directory,
                        _ => return Err(format!("{path}: 模块 {} 缺失或有歧义", child.name).into()),
                    }
                }
            };
            self.file(&child_file, child.module)?;
        }
        Ok(())
    }
}

struct ExternalModule {
    name: String,
    module: Vec<String>,
    inline: Vec<String>,
    path: Option<PathBuf>,
}

struct Dependencies<'a> {
    path: &'a str,
    module: Vec<String>,
    file_module_depth: usize,
    aliases: Aliases,
    findings: &'a mut BTreeSet<String>,
    external: Vec<ExternalModule>,
    unsupported: Vec<String>,
}

impl Dependencies<'_> {
    fn file(&mut self, syntax: &syn::File) {
        if test_only(&syntax.attrs) {
            return;
        }
        self.attributes(&syntax.attrs);
        self.items(&syntax.items);
    }

    fn items(&mut self, items: &[Item]) {
        let previous = self.aliases.clone();
        for item in items {
            if let Item::Use(item) = item
                && !test_only(&item.attrs)
            {
                self.imports(&item.tree, Vec::new(), false);
            }
        }
        for item in items {
            self.visit_item(item);
        }
        self.aliases = previous;
    }

    fn report(&mut self, rule: &str, target: &str) {
        self.findings
            .insert(format!("{}|{rule}|{target}", self.path));
    }

    fn dependency(&mut self, path: &[String]) {
        let Some(from) = self.module.get(2).and_then(|s| rank(s)) else {
            return;
        };
        if path.first().is_none_or(|s| s != "crate") || path.get(1).is_none_or(|s| s != "ui") {
            return;
        }
        let Some(to) = path.get(2).and_then(|s| rank(s)) else {
            return;
        };
        let across_features = self.module.get(2).is_some_and(|s| s == "features")
            && path.get(2).is_some_and(|s| s == "features")
            && self
                .module
                .get(3)
                .zip(path.get(3))
                .is_some_and(|(a, b)| a != b);
        if to < from || across_features {
            self.report("FOLIO-ARCH-002", &path.join("::"));
        }
    }

    fn resolved(&self, path: &[String]) -> Vec<String> {
        let mut path = path.to_vec();
        let mut seen = BTreeSet::new();
        while let Some(head) = path.first() {
            if !seen.insert(head.clone()) {
                break;
            }
            let Some(prefix) = self.aliases.get(head) else {
                break;
            };
            path = prefix.iter().chain(path.iter().skip(1)).cloned().collect();
        }
        match path.first().map(String::as_str) {
            Some("crate") => path,
            Some("self") => self
                .module
                .iter()
                .chain(path.iter().skip(1))
                .cloned()
                .collect(),
            Some("super") => {
                let count = path.iter().take_while(|p| *p == "super").count();
                let end = self.module.len().saturating_sub(count).max(1);
                self.module[..end]
                    .iter()
                    .chain(path.iter().skip(count))
                    .cloned()
                    .collect()
            }
            _ => path,
        }
    }

    fn imports(&mut self, tree: &UseTree, mut prefix: Vec<String>, check: bool) {
        match tree {
            UseTree::Path(node) => {
                prefix.push(node.ident.to_string());
                self.imports(&node.tree, prefix, check);
            }
            UseTree::Group(node) => {
                for tree in &node.items {
                    self.imports(tree, prefix.clone(), check);
                }
            }
            UseTree::Name(node) => {
                let name = node.ident.to_string();
                if name != "self" {
                    prefix.push(name.clone());
                }
                let resolved = self.resolved(&prefix);
                if check {
                    self.dependency(&resolved);
                }
                let alias = if name == "self" {
                    prefix.last().cloned()
                } else {
                    Some(name)
                };
                if let Some(alias) = alias {
                    self.aliases.insert(alias, resolved);
                }
            }
            UseTree::Rename(node) => {
                if node.ident != "self" {
                    prefix.push(node.ident.to_string());
                }
                let resolved = self.resolved(&prefix);
                if check {
                    self.dependency(&resolved);
                }
                self.aliases.insert(node.rename.to_string(), resolved);
            }
            UseTree::Glob(_) => {
                if check {
                    self.dependency(&self.resolved(&prefix));
                }
            }
        }
    }

    fn attributes(&mut self, attrs: &[Attribute]) {
        if self
            .module
            .get(2)
            .is_some_and(|layer| layer == "host" || layer == "platform")
        {
            return;
        }
        if attrs.iter().any(|attr| active_mobile(&attr.meta)) {
            self.report("FOLIO-ARCH-003", "cfg(mobile)");
        }
    }
}

impl<'ast> Visit<'ast> for Dependencies<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let attrs: &[Attribute] = match item {
            Item::Const(i) => &i.attrs,
            Item::Enum(i) => &i.attrs,
            Item::ExternCrate(i) => &i.attrs,
            Item::Fn(i) => &i.attrs,
            Item::ForeignMod(i) => &i.attrs,
            Item::Impl(i) => &i.attrs,
            Item::Macro(i) => &i.attrs,
            Item::Mod(i) => &i.attrs,
            Item::Static(i) => &i.attrs,
            Item::Struct(i) => &i.attrs,
            Item::Trait(i) => &i.attrs,
            Item::TraitAlias(i) => &i.attrs,
            Item::Type(i) => &i.attrs,
            Item::Union(i) => &i.attrs,
            Item::Use(i) => &i.attrs,
            _ => &[],
        };
        if test_only(attrs) {
            return;
        }
        self.attributes(attrs);
        visit::visit_item(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.attrs.iter().any(|attr| conditional_path(&attr.meta)) {
            self.unsupported.push(format!(
                "{}: 模块 {} 的 cfg_attr(path) 尚不支持，不能跳过条件源码",
                self.path, item.ident
            ));
            return;
        }
        let previous = self.module.clone();
        let aliases = std::mem::take(&mut self.aliases);
        self.module.push(item.ident.to_string());
        if let Some((_, items)) = &item.content {
            self.items(items);
        } else {
            let inline = previous
                .iter()
                .skip(self.file_module_depth)
                .cloned()
                .collect();
            let path = item.attrs.iter().find_map(|attr| {
                if let Meta::NameValue(value) = &attr.meta
                    && value.path.is_ident("path")
                    && let syn::Expr::Lit(value) = &value.value
                    && let syn::Lit::Str(path) = &value.lit
                {
                    Some(PathBuf::from(path.value()))
                } else {
                    None
                }
            });
            self.external.push(ExternalModule {
                name: item.ident.to_string(),
                module: self.module.clone(),
                inline,
                path,
            });
        }
        self.module = previous;
        self.aliases = aliases;
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.imports(&item.tree, Vec::new(), true);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let parts: Vec<_> = path.segments.iter().map(|p| p.ident.to_string()).collect();
        self.dependency(&self.resolved(&parts));
        visit::visit_path(self, path);
    }

    fn visit_visibility(&mut self, _: &'ast syn::Visibility) {}

    fn visit_attribute(&mut self, _: &'ast Attribute) {}

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[Attribute] = match item {
            syn::ImplItem::Const(i) => &i.attrs,
            syn::ImplItem::Fn(i) => &i.attrs,
            syn::ImplItem::Type(i) => &i.attrs,
            syn::ImplItem::Macro(i) => &i.attrs,
            _ => &[],
        };
        if test_only(attrs) {
            return;
        }
        self.attributes(attrs);
        visit::visit_impl_item(self, item);
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs: &[Attribute] = match item {
            syn::TraitItem::Const(i) => &i.attrs,
            syn::TraitItem::Fn(i) => &i.attrs,
            syn::TraitItem::Type(i) => &i.attrs,
            syn::TraitItem::Macro(i) => &i.attrs,
            _ => &[],
        };
        if test_only(attrs) {
            return;
        }
        self.attributes(attrs);
        visit::visit_trait_item(self, item);
    }

    fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
        let attrs: &[Attribute] = match item {
            syn::ForeignItem::Fn(i) => &i.attrs,
            syn::ForeignItem::Static(i) => &i.attrs,
            syn::ForeignItem::Type(i) => &i.attrs,
            syn::ForeignItem::Macro(i) => &i.attrs,
            _ => &[],
        };
        if test_only(attrs) {
            return;
        }
        self.attributes(attrs);
        visit::visit_foreign_item(self, item);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if test_only(&local.attrs) {
            return;
        }
        self.attributes(&local.attrs);
        visit::visit_local(self, local);
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if test_only(&field.attrs) {
            return;
        }
        self.attributes(&field.attrs);
        visit::visit_field(self, field);
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if test_only(&arm.attrs) {
            return;
        }
        self.attributes(&arm.attrs);
        visit::visit_arm(self, arm);
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        if test_only(&variant.attrs) {
            return;
        }
        self.attributes(&variant.attrs);
        visit::visit_variant(self, variant);
    }

    fn visit_stmt_macro(&mut self, statement: &'ast syn::StmtMacro) {
        if test_only(&statement.attrs) {
            return;
        }
        self.attributes(&statement.attrs);
        visit::visit_stmt_macro(self, statement);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        let previous = self.aliases.clone();
        for statement in &block.stmts {
            if let syn::Stmt::Item(Item::Use(item)) = statement
                && !test_only(&item.attrs)
            {
                self.imports(&item.tree, Vec::new(), false);
            }
        }
        visit::visit_block(self, block);
        self.aliases = previous;
    }

    fn visit_expr(&mut self, expression: &'ast syn::Expr) {
        let attrs: &[Attribute] = match expression {
            syn::Expr::Array(i) => &i.attrs,
            syn::Expr::Assign(i) => &i.attrs,
            syn::Expr::Async(i) => &i.attrs,
            syn::Expr::Await(i) => &i.attrs,
            syn::Expr::Binary(i) => &i.attrs,
            syn::Expr::Block(i) => &i.attrs,
            syn::Expr::Break(i) => &i.attrs,
            syn::Expr::Call(i) => &i.attrs,
            syn::Expr::Cast(i) => &i.attrs,
            syn::Expr::Closure(i) => &i.attrs,
            syn::Expr::Const(i) => &i.attrs,
            syn::Expr::Continue(i) => &i.attrs,
            syn::Expr::Field(i) => &i.attrs,
            syn::Expr::ForLoop(i) => &i.attrs,
            syn::Expr::Group(i) => &i.attrs,
            syn::Expr::If(i) => &i.attrs,
            syn::Expr::Index(i) => &i.attrs,
            syn::Expr::Infer(i) => &i.attrs,
            syn::Expr::Let(i) => &i.attrs,
            syn::Expr::Lit(i) => &i.attrs,
            syn::Expr::Loop(i) => &i.attrs,
            syn::Expr::Macro(i) => &i.attrs,
            syn::Expr::Match(i) => &i.attrs,
            syn::Expr::MethodCall(i) => &i.attrs,
            syn::Expr::Paren(i) => &i.attrs,
            syn::Expr::Path(i) => &i.attrs,
            syn::Expr::Range(i) => &i.attrs,
            syn::Expr::RawAddr(i) => &i.attrs,
            syn::Expr::Reference(i) => &i.attrs,
            syn::Expr::Repeat(i) => &i.attrs,
            syn::Expr::Return(i) => &i.attrs,
            syn::Expr::Struct(i) => &i.attrs,
            syn::Expr::Try(i) => &i.attrs,
            syn::Expr::TryBlock(i) => &i.attrs,
            syn::Expr::Tuple(i) => &i.attrs,
            syn::Expr::Unary(i) => &i.attrs,
            syn::Expr::Unsafe(i) => &i.attrs,
            syn::Expr::While(i) => &i.attrs,
            syn::Expr::Yield(i) => &i.attrs,
            _ => &[],
        };
        if test_only(attrs) {
            return;
        }
        self.attributes(attrs);
        visit::visit_expr(self, expression);
    }

    fn visit_macro(&mut self, expression: &'ast syn::Macro) {
        if expression.path.is_ident("cfg")
            && let Ok(meta) = syn::parse2::<Meta>(expression.tokens.clone())
            && contains_mobile(&meta)
            && self
                .module
                .get(2)
                .is_none_or(|layer| layer != "host" && layer != "platform")
        {
            self.report("FOLIO-ARCH-003", "cfg!(mobile)");
        }
        visit::visit_macro(self, expression);
    }
}

fn rank(layer: &str) -> Option<u8> {
    match layer {
        "shell" => Some(0),
        "features" => Some(1),
        "components" => Some(2),
        "services" => Some(3),
        "foundation" => Some(4),
        _ => None,
    }
}

fn nested(meta: &Meta) -> Vec<Meta> {
    match meta {
        Meta::List(list) => Punctuated::<Meta, Token![,]>::parse_terminated
            .parse2(list.tokens.clone())
            .map(|m| m.into_iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// (可能为 false, 可能为 true)，仅固定 test=false，其它目标条件保持未知。
fn production(meta: &Meta) -> (bool, bool) {
    if meta.path().is_ident("test") {
        return (true, false);
    }
    let children = nested(meta);
    if meta.path().is_ident("not") && children.len() == 1 {
        let (no, yes) = production(&children[0]);
        return (yes, no);
    }
    if meta.path().is_ident("all") {
        return (
            children.iter().any(|m| production(m).0),
            children.iter().all(|m| production(m).1),
        );
    }
    if meta.path().is_ident("any") {
        return (
            children.iter().all(|m| production(m).0),
            children.iter().any(|m| production(m).1),
        );
    }
    (true, true)
}

fn disables_production(meta: &Meta) -> bool {
    let children = nested(meta);
    if meta.path().is_ident("cfg") {
        return children.first().is_some_and(|m| !production(m).1);
    }
    meta.path().is_ident("cfg_attr")
        && children.first().is_some_and(|m| !production(m).0)
        && children.iter().skip(1).any(disables_production)
}

fn test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        disables_production(&attr.meta)
            || attr.path().is_ident("test")
            || (attr.path().segments.len() == 2
                && attr
                    .path()
                    .segments
                    .first()
                    .is_some_and(|s| s.ident == "gpui_kit")
                && attr
                    .path()
                    .segments
                    .last()
                    .is_some_and(|s| s.ident == "test"))
    })
}

fn contains_mobile(meta: &Meta) -> bool {
    meta.path().is_ident("mobile") || nested(meta).iter().any(contains_mobile)
}

fn active_mobile(meta: &Meta) -> bool {
    let children = nested(meta);
    if meta.path().is_ident("cfg_attr") {
        return children.first().is_some_and(|m| production(m).1)
            && (children.first().is_some_and(contains_mobile)
                || children.iter().skip(1).any(active_mobile));
    }
    meta.path().is_ident("cfg") && children.iter().any(contains_mobile)
}

fn conditional_path(meta: &Meta) -> bool {
    let children = nested(meta);
    meta.path().is_ident("cfg_attr")
        && children.first().is_some_and(|m| production(m).1)
        && children
            .iter()
            .skip(1)
            .any(|m| m.path().is_ident("path") || conditional_path(m))
}

#[cfg(test)]
mod tests {
    use crate::boundaries::{inspect, scan};

    #[test]
    fn comments_and_strings_are_not_dependencies() {
        let source = "// use crate::ui::shell::AppShell;\nfn f() { let _ = \"cfg(mobile)\"; }";
        assert!(
            inspect("sample.rs", &["crate", "ui", "services"], source)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn imports_aliases_and_relative_paths_reveal_reverse_edges() {
        let source = "use super::super as ui; use ui::shell as shell; fn f(_: shell::AppShell) {}";
        let found = inspect("sample.rs", &["crate", "ui", "features", "reader"], source).unwrap();
        assert!(found.iter().any(|s| s.ends_with("|crate::ui::shell")));
        assert!(
            found
                .iter()
                .any(|s| s.ends_with("|crate::ui::shell::AppShell"))
        );
        let cross = inspect(
            "sample.rs",
            &["crate", "ui", "features", "reader"],
            "use crate::ui::features::shelf::ShelfView;",
        )
        .unwrap();
        assert_eq!(cross.len(), 1);
    }

    #[test]
    fn production_mobile_cfg_is_reported_but_test_only_items_are_excluded() {
        let source = "#[cfg(all(test, not(mobile)))] mod tests { use crate::ui::shell::AppShell; }\n\
            #[test] fn check() { use crate::ui::shell::AppShell; }\n\
            #[cfg_attr(test, cfg(mobile))] fn a() {}\n\
            #[cfg_attr(not(test), cfg(mobile))] fn b() {}\n\
            fn c() { #[cfg(mobile)] { let _ = 1; } }";
        let found = inspect("sample.rs", &["crate", "ui", "services"], source).unwrap();
        assert_eq!(found, ["sample.rs|FOLIO-ARCH-003|cfg(mobile)"]);
        assert!(
            inspect("sample.rs", &["crate", "ui", "host"], source)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn module_walk_requires_sources_and_skips_test_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let ui = dir.path().join("crates/folio-app/src/ui");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::write(
            ui.join("mod.rs"),
            "mod services; #[cfg(test)] mod missing_tests;",
        )
        .unwrap();
        assert!(scan(dir.path()).is_err());
        std::fs::write(ui.join("services.rs"), "use crate::ui::shell::AppShell;").unwrap();
        assert_eq!(
            scan(dir.path()).unwrap(),
            ["crates/folio-app/src/ui/services.rs|FOLIO-ARCH-002|crate::ui::shell::AppShell"]
        );
    }

    #[test]
    fn empty_entry_and_conditional_module_paths_cannot_pass_without_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let ui = dir.path().join("crates/folio-app/src/ui");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::write(ui.join("mod.rs"), "// no modules").unwrap();
        assert!(scan(dir.path()).unwrap_err().to_string().contains("空覆盖"));
        assert!(
            inspect(
                "sample.rs",
                &["crate", "ui", "services"],
                "#[cfg_attr(windows, path = \"alternate.rs\")] mod child;"
            )
            .unwrap_err()
            .to_string()
            .contains("cfg_attr(path)")
        );
        assert!(
            inspect(
                "sample.rs",
                &["crate", "ui", "services"],
                "#[cfg_attr(test, path = \"test.rs\")] mod child;"
            )
            .is_ok()
        );
    }
}
