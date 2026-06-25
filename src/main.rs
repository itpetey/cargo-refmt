use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use clap::Parser;
use syn::{Attribute, File, Item, spanned::Spanned, visit::Visit};

mod paths;

type Cat = usize;

#[derive(Parser)]
#[command(name = "refmt")]
#[command(bin_name = "cargo refmt")]
#[command(version, about = "Sort items consistently in Rust source files")]
struct Args {
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct BucketItem {
    sort_key: String,
    snippet: String,
}

struct ImplSnippet {
    target: Option<String>,
    sort_category: usize,
    source_order: usize,
    snippet: String,
}

struct TypeDependencyVisitor<'a> {
    local_types: &'a HashSet<String>,
    dependencies: HashSet<String>,
}

impl Visit<'_> for TypeDependencyVisitor<'_> {
    fn visit_type_path(&mut self, path: &syn::TypePath) {
        for segment in &path.path.segments {
            let ident = segment.ident.to_string();
            if self.local_types.contains(&ident) {
                self.dependencies.insert(ident);
            }
        }

        syn::visit::visit_type_path(self, path);
    }
}

fn main() -> Result<()> {
    let mut raw_args: Vec<String> = std::env::args().collect();
    if raw_args.len() > 1 && raw_args[1] == "refmt" {
        raw_args.remove(1);
    }
    let args = Args::parse_from(raw_args);

    let paths = if args.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.paths
    };

    let input = paths::InputCollector::new()?.collect(paths)?;

    for path in input.iter_active() {
        reorder_file(path).with_context(|| format!("refmt {}", path.display()))?;
    }

    // Run `cargo fmt` last to catch any outstanding formatting issues
    Command::new("cargo").arg("fmt").status()?;

    Ok(())
}

fn blank_lines_after(category: usize) -> usize {
    match category {
        0..=7 => 0,
        8..=12 => 1,
        13 => 1,
        _ => 1,
    }
}

fn category(item: &Item) -> Cat {
    if is_test_module(item) {
        return 12;
    }

    match item {
        Item::Use(use_item) => {
            if matches!(use_item.vis, syn::Visibility::Public(_)) {
                3
            } else {
                use_category(use_item)
            }
        }
        Item::Mod(_) => 4,
        Item::ExternCrate(_) => 5,
        Item::Type(_) => 6,
        Item::Const(_) | Item::Static(_) => 7,
        Item::Trait(_) | Item::TraitAlias(_) => 8,
        Item::Struct(s) => {
            if matches!(s.vis, syn::Visibility::Public(_)) {
                9
            } else {
                13
            }
        }
        Item::Enum(e) => {
            if matches!(e.vis, syn::Visibility::Public(_)) {
                9
            } else {
                13
            }
        }
        Item::Union(u) => {
            if matches!(u.vis, syn::Visibility::Public(_)) {
                9
            } else {
                13
            }
        }
        Item::Impl(_) => 10,
        Item::Fn(_) | Item::ForeignMod(_) | Item::Macro(_) | Item::Verbatim(_) => 11,
        _ => 11,
    }
}

fn collect_impls_and_bucket_rest(
    items: Vec<Item>,
    buckets: &mut [Vec<BucketItem>],
    src: &str,
    line_starts: &[usize],
) -> Vec<ImplSnippet> {
    let mut impls = Vec::new();
    let local_traits = items
        .iter()
        .filter_map(|item| match item {
            Item::Trait(item) => Some(item.ident.to_string()),
            Item::TraitAlias(item) => Some(item.ident.to_string()),
            _ => None,
        })
        .collect::<HashSet<_>>();

    for (source_order, item) in items.into_iter().enumerate() {
        if let Item::Impl(impl_item) = &item {
            impls.push(ImplSnippet {
                target: impl_type_name(impl_item),
                sort_category: impl_sort_category(impl_item, &local_traits),
                source_order,
                snippet: item_snippet(&item, src, line_starts),
            });
        } else {
            let cat = category(&item);
            buckets[cat].push(BucketItem {
                sort_key: item_sort_key(&item),
                snippet: item_snippet(&item, src, line_starts),
            });
        }
    }

    impls
}

fn collect_type_item_dependencies(item: &Item, local_types: &HashSet<String>) -> HashSet<String> {
    let mut visitor = TypeDependencyVisitor {
        local_types,
        dependencies: HashSet::new(),
    };

    match item {
        Item::Struct(item) => visitor.visit_fields(&item.fields),
        Item::Enum(item) => {
            for variant in &item.variants {
                visitor.visit_fields(&variant.fields);
            }
        }
        Item::Union(item) => visitor.visit_fields_named(&item.fields),
        _ => {}
    }

    if let Some(name) = item_name(item) {
        visitor.dependencies.remove(&name);
    }

    visitor.dependencies
}

fn contains_test(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Path(path) => path.path.is_ident("test"),
        syn::Expr::Tuple(tuple) => tuple.elems.iter().any(contains_test),
        syn::Expr::Binary(bin) => contains_test(&bin.left) || contains_test(&bin.right),
        syn::Expr::Group(group) => contains_test(&group.expr),
        syn::Expr::Call(call) => {
            if let syn::Expr::Path(path) = &*call.func
                && (path.path.is_ident("any") || path.path.is_ident("all"))
            {
                return call.args.iter().any(contains_test);
            }
            false
        }
        _ => false,
    }
}

fn fn_item_name(item: &Item) -> String {
    match item {
        Item::Fn(fn_item) => fn_item.sig.ident.to_string(),
        _ => String::new(),
    }
}

fn fn_visibility_rank(item: &Item) -> u8 {
    match item {
        Item::Fn(fn_item) => match &fn_item.vis {
            syn::Visibility::Public(_) => 0,
            syn::Visibility::Restricted(_) => 1,
            syn::Visibility::Inherited => 2,
        },
        _ => 0,
    }
}

fn format_use_inline(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(path) => {
            let rest = format_use_inline(&path.tree);
            format!("{}::{}", path.ident, rest)
        }
        syn::UseTree::Name(name) => name.ident.to_string(),
        syn::UseTree::Rename(rename) => format!("{} as {}", rename.ident, rename.rename),
        syn::UseTree::Glob(_) => "*".to_string(),
        syn::UseTree::Group(group) => {
            let items: Vec<_> = group.items.iter().map(format_use_inline).collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

fn format_use_multi_line(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(path) => {
            if let syn::UseTree::Group(group) = &*path.tree {
                let mut inner = format!("{}::{{\n", path.ident);
                for item in &group.items {
                    inner.push_str("    ");
                    inner.push_str(&format_use_inline(item));
                    inner.push_str(",\n");
                }
                inner.push('}');
                inner
            } else {
                let rest = format_use_multi_line(&path.tree);
                format!("{}::{}", path.ident, rest)
            }
        }
        syn::UseTree::Name(name) => name.ident.to_string(),
        syn::UseTree::Rename(rename) => format!("{} as {}", rename.ident, rename.rename),
        syn::UseTree::Glob(_) => "*".to_string(),
        syn::UseTree::Group(group) => {
            let items: Vec<_> = group.items.iter().map(format_use_inline).collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        match attr.parse_args::<syn::Expr>() {
            Ok(expr) => contains_test(&expr),
            Err(_) => false,
        }
    })
}

fn header_to_string(attrs: &[Attribute], src: &str, line_starts: &[usize]) -> String {
    if attrs.is_empty() {
        return String::new();
    }

    let mut start = usize::MAX;
    let mut end = 0usize;

    for attr in attrs {
        let range = span_range(attr.span(), line_starts, src.len());
        start = start.min(range.start);
        end = end.max(range.end);
    }

    src[start..end].to_string()
}

fn impl_sort_category(impl_item: &syn::ItemImpl, local_traits: &HashSet<String>) -> usize {
    match &impl_item.trait_ {
        None => 0,
        Some((_, trait_path, _)) => {
            if is_rust_trait(trait_path, local_traits) {
                2
            } else {
                1
            }
        }
    }
}

fn impl_target_matches_type(target: Option<&str>, type_name: &str) -> bool {
    let Some(target) = target else {
        return false;
    };

    if target == type_name {
        return true;
    }

    let segments = target.split("::").collect::<Vec<_>>();
    matches!(segments.first(), Some(&"crate" | &"self" | &"super"))
        && segments.last().is_some_and(|segment| *segment == type_name)
}

fn impl_type_name(impl_item: &syn::ItemImpl) -> Option<String> {
    type_key(&impl_item.self_ty)
}

fn is_rust_trait(trait_path: &syn::Path, local_traits: &HashSet<String>) -> bool {
    let std_traits = [
        "Drop",
        "Clone",
        "Copy",
        "Debug",
        "Display",
        "From",
        "Into",
        "AsRef",
        "AsMut",
        "Deref",
        "DerefMut",
        "Iterator",
        "IntoIterator",
        "Eq",
        "PartialEq",
        "Ord",
        "PartialOrd",
        "Hash",
        "Default",
        "Send",
        "Sync",
        "Fn",
        "FnOnce",
        "FnMut",
        "Future",
        "Stream",
        "Error",
        "ToOwned",
        "Borrow",
        "BorrowMut",
        "AsHandle",
        "AsRawHandle",
        "FromStr",
        "TryFrom",
        "TryInto",
        "Index",
        "IndexMut",
    ];

    let first = trait_path
        .segments
        .first()
        .map(|segment| segment.ident.to_string());
    let last = trait_path
        .segments
        .last()
        .map(|segment| segment.ident.to_string());

    if matches!(first.as_deref(), Some("std" | "core" | "alloc")) {
        return true;
    }

    last.as_deref().is_some_and(|trait_name| {
        !local_traits.contains(trait_name) && std_traits.contains(&trait_name)
    })
}

fn is_std_crate(name: &str) -> bool {
    name == "std"
        || name == "core"
        || name == "alloc"
        || name.starts_with("std::")
        || name.starts_with("core::")
        || name.starts_with("alloc::")
}

fn is_test_module(item: &Item) -> bool {
    match item {
        Item::Mod(module) => has_cfg_test(&module.attrs),
        _ => false,
    }
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

fn item_name(item: &Item) -> Option<String> {
    match item {
        Item::Struct(s) => Some(s.ident.to_string()),
        Item::Enum(e) => Some(e.ident.to_string()),
        Item::Union(u) => Some(u.ident.to_string()),
        _ => None,
    }
}

fn item_is_public_type(item: &Item) -> bool {
    match item {
        Item::Struct(item) => matches!(item.vis, syn::Visibility::Public(_)),
        Item::Enum(item) => matches!(item.vis, syn::Visibility::Public(_)),
        Item::Union(item) => matches!(item.vis, syn::Visibility::Public(_)),
        _ => false,
    }
}

fn item_snippet(item: &Item, src: &str, line_starts: &[usize]) -> String {
    let range = item_snippet_byte_range(item, src, line_starts);
    src[range].trim_end().to_string()
}

/// Returns the byte range of the item's snippet in the original source,
/// extended backward to include any preceding attributes and comments.
fn item_snippet_byte_range(
    item: &Item,
    src: &str,
    line_starts: &[usize],
) -> std::ops::Range<usize> {
    let mut range = span_range(item.span(), line_starts, src.len());

    for attr in item_attributes(item) {
        let attr_range = span_range(attr.span(), line_starts, src.len());
        if attr_range.start < range.start {
            range.start = attr_range.start;
        }
    }

    range.start = range.start.min(range.end);

    // Extend range backward to include any immediately preceding comments
    range.start = preceding_comment_start(src, range.start);

    range
}

/// Walk backward from `start` to find where preceding line comments begin.
/// Returns the byte offset of the first comment line that is part of the
/// contiguous block of comments immediately before `start`.
fn preceding_comment_start(src: &str, start: usize) -> usize {
    // Find the start of the line containing `start`
    let line_start = src[..start].rfind('\n').map(|pos| pos + 1).unwrap_or(0);

    // Walk backward line by line looking for comments
    let mut current_line_start = line_start;
    let mut comment_block_start = line_start;

    loop {
        // Find the previous line
        if current_line_start == 0 {
            break;
        }

        // Go back to find the end of the previous line (before the newline)
        let prev_newline = src[..current_line_start - 1].rfind('\n');
        let prev_line_start = prev_newline.map(|pos| pos + 1).unwrap_or(0);
        let prev_line_end = current_line_start - 1; // exclude the newline

        let prev_line = &src[prev_line_start..prev_line_end];
        let trimmed = prev_line.trim();

        if trimmed.starts_with("//") {
            // Line comment - include it
            comment_block_start = prev_line_start;
            current_line_start = prev_line_start;
        } else if trimmed.is_empty() {
            // Blank line - stop looking
            break;
        } else if trimmed.ends_with("*/") {
            // End of a block comment - find its start
            if let Some(block_start) = find_block_comment_start(src, prev_line_start, prev_line_end)
            {
                comment_block_start = block_start;
                current_line_start = block_start;
            } else {
                break;
            }
        } else {
            // Non-comment line - stop
            break;
        }
    }

    comment_block_start
}

/// Find the start of a block comment that ends at or before `end`.
/// Searches backward from `end` through the source to find the opening `/*`.
fn find_block_comment_start(src: &str, _search_start: usize, end: usize) -> Option<usize> {
    // Search backward through the entire source up to `end` for the opening /*
    let search_region = &src[..end];
    let pos = search_region.rfind("/*")?;
    Some(pos)
}

fn item_sort_key(item: &Item) -> String {
    match item {
        Item::Use(use_item) => use_path_to_string(&use_item.tree),
        Item::Mod(mod_item) => mod_item.ident.to_string(),
        Item::ExternCrate(extern_crate) => extern_crate.ident.to_string(),
        Item::Type(type_item) => type_item.ident.to_string(),
        Item::Const(const_item) => const_item.ident.to_string(),
        Item::Static(static_item) => static_item.ident.to_string(),
        Item::Trait(trait_item) => trait_item.ident.to_string(),
        Item::TraitAlias(trait_alias) => trait_alias.ident.to_string(),
        Item::Struct(struct_item) => struct_item.ident.to_string(),
        Item::Enum(enum_item) => enum_item.ident.to_string(),
        Item::Union(union_item) => union_item.ident.to_string(),
        Item::Impl(impl_item) => impl_type_name(impl_item).unwrap_or_default(),
        Item::Fn(fn_item) => fn_item.sig.ident.to_string(),
        Item::ForeignMod(_) => String::new(),
        Item::Macro(macro_item) => macro_item
            .mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        Item::Verbatim(_) => String::new(),
        _ => String::new(),
    }
}

fn line_start_offsets(src: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(src.len() / 32 + 2);
    starts.push(0);
    for (idx, ch) in src.char_indices() {
        if ch == '\n' {
            let next = idx + ch.len_utf8();
            starts.push(next);
        }
    }
    if *starts.last().unwrap_or(&0) != src.len() {
        starts.push(src.len());
    }
    starts
}

fn merge_use_trees(snippets: &[String]) -> Option<Vec<String>> {
    let mut roots: HashMap<String, Vec<(syn::UseTree, bool)>> = HashMap::new();

    for snippet in snippets {
        let file = syn::parse_file(snippet).ok()?;
        let item = file.items.into_iter().next()?;
        let Item::Use(use_item) = item else {
            return None;
        };
        if !use_item.attrs.is_empty() {
            return None;
        }
        let root = use_tree_root(&use_item.tree)?;
        let is_bare = !matches!(use_item.tree, syn::UseTree::Path(_));
        let rest = match use_item.tree {
            syn::UseTree::Path(path) => *path.tree,
            other => other,
        };
        roots.entry(root).or_default().push((rest, is_bare));
    }

    let mut result = Vec::new();
    let mut sorted_roots: Vec<_> = roots.into_iter().collect();
    sorted_roots.sort_by_key(|(root, _)| root.clone());

    for (root, subtrees) in sorted_roots {
        let (bare, path_imports): (Vec<_>, Vec<_>) =
            subtrees.into_iter().partition(|(_, is_bare)| *is_bare);

        for (bare_tree, _) in bare {
            result.push(format_use_multi_line(&bare_tree));
        }

        let path_trees: Vec<syn::UseTree> = path_imports.into_iter().map(|(t, _)| t).collect();

        if path_trees.is_empty() {
            continue;
        }

        let merged_tree = if path_trees.len() == 1 {
            syn::UseTree::Path(syn::UsePath {
                ident: syn::Ident::new(&root, proc_macro2::Span::call_site()),
                colon2_token: syn::Token![::](proc_macro2::Span::call_site()),
                tree: Box::new(path_trees.into_iter().next().unwrap()),
            })
        } else {
            let mut items: Vec<syn::UseTree> = Vec::new();
            for subtree in path_trees {
                if let syn::UseTree::Group(g) = subtree {
                    items.extend(g.items);
                } else {
                    items.push(subtree);
                }
            }
            items.sort_by(|a, b| use_path_to_string(a).cmp(&use_path_to_string(b)));
            let mut punctuated: syn::punctuated::Punctuated<syn::UseTree, syn::Token![,]> =
                syn::punctuated::Punctuated::new();
            for item in items {
                punctuated.push(item);
            }
            syn::UseTree::Path(syn::UsePath {
                ident: syn::Ident::new(&root, proc_macro2::Span::call_site()),
                colon2_token: syn::Token![::](proc_macro2::Span::call_site()),
                tree: Box::new(syn::UseTree::Group(syn::UseGroup {
                    brace_token: syn::token::Brace(proc_macro2::Span::call_site()),
                    items: punctuated,
                })),
            })
        };
        result.push(format_use_multi_line(&merged_tree));
    }

    Some(result)
}

fn path_key(path: &syn::Path) -> Option<String> {
    let segments = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("::"))
    }
}

fn push_ordered_impls(
    impls: Vec<ImplSnippet>,
    type_order: &[String],
    bucket: &mut Vec<BucketItem>,
) {
    let mut used_impls = vec![false; impls.len()];

    for type_name in type_order {
        let mut matching_impls = impls
            .iter()
            .enumerate()
            .filter(|(_, impl_item)| {
                impl_target_matches_type(impl_item.target.as_deref(), type_name)
            })
            .collect::<Vec<_>>();
        matching_impls
            .sort_by_key(|(_, impl_item)| (impl_item.sort_category, impl_item.source_order));

        for (index, impl_item) in matching_impls {
            used_impls[index] = true;
            bucket.push(BucketItem {
                sort_key: impl_item.target.clone().unwrap_or_default(),
                snippet: impl_item.snippet.clone(),
            });
        }
    }

    for (index, impl_item) in impls.into_iter().enumerate() {
        if !used_impls[index] {
            bucket.push(BucketItem {
                sort_key: impl_item.target.unwrap_or_default(),
                snippet: impl_item.snippet,
            });
        }
    }
}

fn push_type_items(
    items: Vec<Item>,
    buckets: &mut [Vec<BucketItem>],
    src: &str,
    line_starts: &[usize],
) {
    for item in items {
        buckets[9].push(BucketItem {
            sort_key: item_sort_key(&item),
            snippet: item_snippet(&item, src, line_starts),
        });
    }
}

fn reorder_file(path: &Path) -> Result<()> {
    let src = fs::read_to_string(path).with_context(|| format!("read file {}", path.display()))?;
    let mut file: File =
        syn::parse_file(&src).with_context(|| format!("parse {}", path.display()))?;
    let line_starts = line_start_offsets(&src);

    let shebang = file.shebang.take();
    let crate_attrs = std::mem::take(&mut file.attrs);

    // Honour file-level #![rustfmt::skip] — skip the entire file.
    if paths::has_rustfmt_skip(&crate_attrs) {
        return Ok(());
    }

    let file_name = path.file_name().and_then(|n| n.to_str());

    let is_main_rs = file_name == Some("main.rs")
        && path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("src");

    let is_build_rs = file_name == Some("build.rs")
        && !path
            .ancestors()
            .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("src"));

    let is_entry_point = is_main_rs || is_build_rs;

    // Break items into segments separated by #[rustfmt::skip] items.
    // Each segment of consecutive non-skip items is reordered independently;
    // skip items are kept verbatim at their original positions.
    enum Segment {
        Skip {
            snippet: String,
            leading: String,
            trailing: String,
        },
        Process(Vec<Item>),
    }

    let mut segments: Vec<Segment> = Vec::new();
    let mut pending: Vec<Item> = Vec::new();
    // Byte offset in `src` right after the last iterated item's snippet end.
    // Used to compute the leading gap for skip items.
    let mut prev_item_end: Option<usize> = None;
    // Pre-compute snippet byte ranges for all items so we can look at neighbours.
    let item_info: Vec<(std::ops::Range<usize>, Item)> = file
        .items
        .iter()
        .map(|item| {
            let range = item_snippet_byte_range(item, &src, &line_starts);
            (range, item.clone()) // Item is cheap to clone (it's an AST)
        })
        .collect();

    for (i, (item_range, item)) in item_info.into_iter().enumerate() {
        let snippet_text = src[item_range.clone()].trim_end().to_string();

        if paths::has_rustfmt_skip(item_attributes(&item)) {
            if !pending.is_empty() {
                segments.push(Segment::Process(std::mem::take(&mut pending)));
            }
            // Capture original leading whitespace between previous item and this skip
            let leading = match prev_item_end {
                Some(end) => {
                    let raw_gap = &src[end..item_range.start];
                    // Strip one leading \n — the line terminator of the previous
                    // item, which is already provided by write_bucket.
                    raw_gap.strip_prefix('\n').unwrap_or(raw_gap).to_string()
                }
                None => String::new(),
            };
            // Capture original trailing whitespace between this skip and the next item.
            // If the next item is also a skip, its leading will handle the spacing
            // so we leave trailing empty to avoid double-counting.
            let trailing = file
                .items
                .get(i + 1)
                .map(|next| {
                    if paths::has_rustfmt_skip(item_attributes(next)) {
                        return String::new();
                    }
                    let next_range = item_snippet_byte_range(next, &src, &line_starts);
                    let raw_gap = &src[item_range.end..next_range.start];
                    // Strip one leading \n — the line terminator of this skip's
                    // last line, which is already added by the output builder.
                    raw_gap.strip_prefix('\n').unwrap_or(raw_gap).to_string()
                })
                .unwrap_or_default();
            segments.push(Segment::Skip {
                snippet: snippet_text,
                leading,
                trailing,
            });
            prev_item_end = Some(item_range.end);
        } else {
            pending.push(item);
            prev_item_end = Some(item_range.end);
        }
    }
    if !pending.is_empty() {
        segments.push(Segment::Process(pending));
    }

    // Build output from segments
    let mut out = String::new();
    if let Some(sb) = shebang {
        out.push_str(&sb);
        out.push('\n');
    }
    if !crate_attrs.is_empty() {
        let header = header_to_string(&crate_attrs, &src, &line_starts);
        out.push_str(header.trim_end());
        out.push_str("\n\n");
    }

    let mut wrote_any = !out.is_empty();
    let mut prev_was_skip = false;

    for segment in segments {
        match segment {
            Segment::Skip {
                snippet,
                leading,
                trailing,
            } => {
                if wrote_any && !leading.is_empty() {
                    out.push_str(&leading);
                }
                out.push_str(&snippet);
                out.push('\n');
                if !trailing.is_empty() {
                    out.push_str(&trailing);
                }
                wrote_any = true;
                prev_was_skip = true;
            }
            Segment::Process(items) => {
                let reordered = reorder_items_to_string(items, &src, &line_starts, is_entry_point);
                if !reordered.is_empty() {
                    if wrote_any && !prev_was_skip {
                        while !out.ends_with("\n\n") {
                            out.push('\n');
                        }
                    }
                    out.push_str(&reordered);
                    wrote_any = true;
                    prev_was_skip = false;
                }
            }
        }
    }

    while out.ends_with("\n\n\n") {
        out.pop();
    }
    let src_has_trailing_newline = src.ends_with('\n');
    let out_has_trailing_newline = out.ends_with('\n');
    if src_has_trailing_newline && !out_has_trailing_newline {
        out.push('\n');
    } else if !src_has_trailing_newline && out_has_trailing_newline {
        out.pop();
    }

    if out != src {
        fs::write(path, out)?;
    }

    Ok(())
}

fn reorder_items_to_string(
    items: Vec<Item>,
    src: &str,
    line_starts: &[usize],
    is_entry_point: bool,
) -> String {
    let (struct_enum_items, rest_items): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| matches!(item, Item::Struct(_) | Item::Enum(_) | Item::Union(_)));

    let (fn_items, other_items): (Vec<_>, Vec<_>) = rest_items
        .into_iter()
        .partition(|item| matches!(item, Item::Fn(_)));

    let sorted_struct_enums = sort_type_items_by_dependencies(struct_enum_items);

    let mut sorted_fn_items = fn_items;
    sorted_fn_items.sort_by(|a, b| {
        if is_entry_point {
            let a_is_main = fn_item_name(a) == "main";
            let b_is_main = fn_item_name(b) == "main";
            if a_is_main && !b_is_main {
                return std::cmp::Ordering::Less;
            }
            if !a_is_main && b_is_main {
                return std::cmp::Ordering::Greater;
            }
        }
        fn_visibility_rank(a)
            .cmp(&fn_visibility_rank(b))
            .then_with(|| fn_item_name(a).cmp(&fn_item_name(b)))
    });

    let mut buckets: Vec<Vec<BucketItem>> = (0..14).map(|_| Vec::new()).collect();

    let impl_items = collect_impls_and_bucket_rest(other_items, &mut buckets, src, line_starts);

    let type_order: Vec<String> = sorted_struct_enums
        .iter()
        .filter_map(|item| item_name(item))
        .collect();

    push_type_items(sorted_struct_enums, &mut buckets, src, line_starts);
    push_ordered_impls(impl_items, &type_order, &mut buckets[10]);

    for item in sorted_fn_items.into_iter() {
        let snippet = item_snippet(&item, src, line_starts);
        buckets[11].push(BucketItem {
            sort_key: item_sort_key(&item),
            snippet,
        });
    }

    let mut out = String::new();
    let mut wrote_any = false;

    let order = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 10, 11, 12];
    for idx in order {
        if let Some(bucket) = buckets.get_mut(idx) {
            write_bucket(&mut out, bucket, idx, &mut wrote_any);
        }
    }

    out
}

fn sort_type_items_by_dependencies(items: Vec<Item>) -> Vec<Item> {
    let local_types = items.iter().filter_map(item_name).collect::<HashSet<_>>();
    let is_public = items.iter().map(item_is_public_type).collect::<Vec<_>>();
    let type_indexes = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item_name(item).map(|name| (name, index)))
        .collect::<HashMap<_, _>>();

    let mut dependency_indexes = vec![HashSet::new(); items.len()];
    for (index, item) in items.iter().enumerate() {
        for dependency in collect_type_item_dependencies(item, &local_types) {
            let Some(dependency_index) = type_indexes.get(&dependency).copied() else {
                continue;
            };

            // Public items should lead their private implementation details.
            if is_public[index] && !is_public[dependency_index] {
                dependency_indexes[dependency_index].insert(index);
            } else {
                dependency_indexes[index].insert(dependency_index);
            }
        }
    }

    let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
    let mut placed = vec![false; items.len()];
    let mut sorted = Vec::with_capacity(items.len());

    while sorted.len() < items.len() {
        let next = (0..items.len())
            .find(|&index| {
                !placed[index]
                    && dependency_indexes[index]
                        .iter()
                        .all(|dependency| placed[*dependency])
            })
            .or_else(|| (0..items.len()).find(|&index| !placed[index]));

        let Some(index) = next else {
            break;
        };

        placed[index] = true;
        sorted.push(items[index].take().expect("type item should be present"));
    }

    sorted
}

fn span_range(
    span: proc_macro2::Span,
    line_starts: &[usize],
    src_len: usize,
) -> std::ops::Range<usize> {
    let start = span.start();
    let end = span.end();

    let start_line_index = start.line.saturating_sub(1);
    let end_line_index = end.line.saturating_sub(1);

    let start_line_base = line_starts
        .get(start_line_index)
        .copied()
        .unwrap_or(src_len);
    let end_line_base = line_starts.get(end_line_index).copied().unwrap_or(src_len);

    let mut start_idx = start_line_base.saturating_add(start.column);
    let mut end_idx = end_line_base.saturating_add(end.column);

    if start_idx > src_len {
        start_idx = src_len;
    }
    if end_idx > src_len {
        end_idx = src_len;
    }

    if start_idx > end_idx {
        start_idx = end_idx;
    }

    start_idx..end_idx
}

fn type_key(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Group(group) => type_key(&group.elem),
        syn::Type::Paren(paren) => type_key(&paren.elem),
        syn::Type::Path(path) => path_key(&path.path),
        syn::Type::Reference(reference) => type_key(&reference.elem),
        _ => None,
    }
}

fn use_category(use_item: &syn::ItemUse) -> Cat {
    fn get_first_ident(tree: &syn::UseTree) -> Option<&syn::Ident> {
        match tree {
            syn::UseTree::Path(tree) => Some(&tree.ident),
            syn::UseTree::Group(tree) => tree.items.first().and_then(|t| get_first_ident(t)),
            syn::UseTree::Name(tree) => Some(&tree.ident),
            syn::UseTree::Rename(_) | syn::UseTree::Glob(_) => None,
        }
    }

    let ident = match get_first_ident(&use_item.tree) {
        Some(id) => id,
        _ => return 1,
    };
    let ident_str = ident.to_string();
    if ident_str == "crate" || ident_str == "self" {
        return 2;
    }
    if is_std_crate(&ident_str) {
        return 0;
    }
    1
}

fn use_path_to_string(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(path) => {
            let rest = use_path_to_string(&path.tree);
            if rest.is_empty() {
                path.ident.to_string()
            } else {
                format!("{}::{}", path.ident, rest)
            }
        }
        syn::UseTree::Name(name) => name.ident.to_string(),
        syn::UseTree::Rename(rename) => rename.ident.to_string(),
        syn::UseTree::Glob(_) => "*".to_string(),
        syn::UseTree::Group(group) => {
            let mut paths: Vec<_> = group.items.iter().map(use_path_to_string).collect();
            paths.sort();
            paths.join(", ")
        }
    }
}

fn use_tree_root(tree: &syn::UseTree) -> Option<String> {
    match tree {
        syn::UseTree::Path(path) => Some(path.ident.to_string()),
        syn::UseTree::Name(name) => Some(name.ident.to_string()),
        syn::UseTree::Rename(rename) => Some(rename.ident.to_string()),
        syn::UseTree::Group(group) => group.items.first().and_then(use_tree_root),
        syn::UseTree::Glob(_) => None,
    }
}

fn write_bucket(
    out: &mut String,
    bucket: &mut Vec<BucketItem>,
    category: usize,
    wrote_any: &mut bool,
) {
    if bucket.is_empty() {
        return;
    }

    if category == 9 || category == 10 || category == 13 {
        // These buckets carry their semantic order from earlier grouping.
    } else if category != 11 {
        bucket.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    }

    if *wrote_any && category != 0 {
        while !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    *wrote_any = true;

    let extra_blank = blank_lines_after(category);
    let bucket_len = bucket.len();

    if matches!(category, 0..=2) {
        let snippets: Vec<_> = bucket
            .drain(..)
            .map(|item| item.snippet.trim_end_matches('\n').to_string())
            .collect();

        if let Some(merged) = merge_use_trees(&snippets) {
            for use_stmt in merged {
                out.push_str("use ");
                out.push_str(&use_stmt);
                out.push_str(";\n");
            }
        } else {
            for (i, snippet) in snippets.iter().enumerate() {
                out.push_str(snippet);
                out.push('\n');
                if i + 1 < snippets.len() {
                    for _ in 0..extra_blank {
                        out.push('\n');
                    }
                }
            }
        }
    } else {
        for (i, item) in bucket.drain(..).enumerate() {
            out.push_str(item.snippet.trim_end_matches('\n'));
            out.push('\n');
            if i + 1 < bucket_len {
                for _ in 0..extra_blank {
                    out.push('\n');
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_start_offsets() {
        let src = "line1\nline2\nline3";
        let starts = line_start_offsets(src);
        assert_eq!(starts, vec![0, 6, 12, 17]);
    }

    #[test]
    fn test_line_start_offsets_empty() {
        let src = "";
        let starts = line_start_offsets(src);
        assert_eq!(starts, vec![0]);
    }

    #[test]
    fn test_line_start_offsets_single_line() {
        let src = "hello";
        let starts = line_start_offsets(src);
        assert_eq!(starts, vec![0, 5]);
    }

    #[test]
    fn test_blank_lines_after() {
        assert_eq!(blank_lines_after(0), 0);
        assert_eq!(blank_lines_after(1), 0);
        assert_eq!(blank_lines_after(2), 0);
        assert_eq!(blank_lines_after(3), 0);
        assert_eq!(blank_lines_after(4), 0);
        assert_eq!(blank_lines_after(5), 0);
        assert_eq!(blank_lines_after(6), 0);
        assert_eq!(blank_lines_after(7), 0);
        assert_eq!(blank_lines_after(8), 1);
        assert_eq!(blank_lines_after(9), 1);
        assert_eq!(blank_lines_after(10), 1);
        assert_eq!(blank_lines_after(11), 1);
        assert_eq!(blank_lines_after(12), 1);
    }
}
