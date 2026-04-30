use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, File, Item};

type Cat = usize;

#[derive(Parser)]
#[command(name = "refmt")]
#[command(bin_name = "cargo refmt")]
#[command(version, about = "Sort items consistently in Rust source files")]
struct Args {
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
}

struct ImplSnippet {
    target: Option<String>,
    sort_category: usize,
    source_order: usize,
    snippet: String,
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

fn collect_directory(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);

    while let Some(current) = queue.pop_front() {
        let mut entries = Vec::new();
        let read_dir = fs::read_dir(&current)
            .with_context(|| format!("read directory {}", current.display()))?;

        for entry in read_dir {
            let entry = entry.with_context(|| format!("read entry in {}", current.display()))?;
            entries.push(entry);
        }

        entries.sort_by_key(|a| a.path());

        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("determine type for {}", path.display()))?;

            if file_type.is_dir() {
                queue.push_back(path);
            } else if file_type.is_file() {
                if is_rust_file(&path) {
                    push_file(path, files, seen);
                }
            } else if file_type.is_symlink() {
                let metadata = fs::metadata(&path)
                    .with_context(|| format!("inspect symlink target {}", path.display()))?;
                if metadata.is_dir() {
                    continue;
                } else if metadata.is_file() && is_rust_file(&path) {
                    push_file(path, files, seen);
                }
            }
        }
    }

    Ok(())
}

fn collect_input_files(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();

    for path in paths {
        collect_path(&path, &mut files, &mut seen)?;
    }

    if files.is_empty() {
        bail!("no Rust files found");
    }

    Ok(files)
}

fn collect_path(path: &Path, files: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) -> Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("inspect metadata for {}", path.display()))?;

    if metadata.is_dir() {
        collect_directory(path, files, seen)?;
    } else if metadata.is_file() {
        push_file(path.to_path_buf(), files, seen);
    }

    Ok(())
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

fn impl_type_name(impl_item: &syn::ItemImpl) -> Option<String> {
    type_key(&impl_item.self_ty)
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

fn type_key(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Group(group) => type_key(&group.elem),
        syn::Type::Paren(paren) => type_key(&paren.elem),
        syn::Type::Path(path) => path_key(&path.path),
        syn::Type::Reference(reference) => type_key(&reference.elem),
        _ => None,
    }
}

fn is_rust_file(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("rs"),
        None => false,
    }
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

fn item_snippet(item: &Item, src: &str, line_starts: &[usize]) -> String {
    let mut range = span_range(item.span(), line_starts, src.len());

    for attr in item_attributes(item) {
        let attr_range = span_range(attr.span(), line_starts, src.len());
        if attr_range.start < range.start {
            range.start = attr_range.start;
        }
    }

    range.start = range.start.min(range.end);

    src[range].trim_end().to_string()
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

    let files = collect_input_files(paths)?;

    for path in files {
        reorder_file(&path).with_context(|| format!("refmt {}", path.display()))?;
    }

    Ok(())
}

fn push_file(path: PathBuf, files: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if seen.insert(path.clone()) {
        files.push(path);
    }
}

fn collect_impls_and_bucket_rest(
    items: Vec<Item>,
    buckets: &mut [Vec<String>],
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
            buckets[cat].push(item_snippet(&item, src, line_starts));
        }
    }

    impls
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

fn sort_type_items_by_dependencies(items: Vec<Item>) -> Vec<Item> {
    let local_types = items.iter().filter_map(item_name).collect::<HashSet<_>>();
    let type_indexes = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item_name(item).map(|name| (name, index)))
        .collect::<HashMap<_, _>>();
    let dependency_indexes = items
        .iter()
        .map(|item| {
            collect_type_item_dependencies(item, &local_types)
                .into_iter()
                .filter_map(|dependency| type_indexes.get(&dependency).copied())
                .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();

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

fn push_ordered_impls(impls: Vec<ImplSnippet>, type_order: &[String], bucket: &mut Vec<String>) {
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
            bucket.push(impl_item.snippet.clone());
        }
    }

    for (index, impl_item) in impls.into_iter().enumerate() {
        if !used_impls[index] {
            bucket.push(impl_item.snippet);
        }
    }
}

fn push_type_items(
    items: Vec<Item>,
    buckets: &mut [Vec<String>],
    src: &str,
    line_starts: &[usize],
) {
    for item in items {
        buckets[9].push(item_snippet(&item, src, line_starts));
    }
}

fn write_bucket(out: &mut String, bucket: &mut Vec<String>, category: usize, wrote_any: &mut bool) {
    if bucket.is_empty() {
        return;
    }

    if category == 9 || category == 10 || category == 13 {
        // These buckets carry their semantic order from earlier grouping.
    } else if category != 11 {
        bucket.sort();
    }

    if *wrote_any && category != 0 {
        while !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    *wrote_any = true;

    let extra_blank = blank_lines_after(category);
    let bucket_len = bucket.len();
    for (i, item) in bucket.drain(..).enumerate() {
        out.push_str(item.trim_end_matches('\n'));
        out.push('\n');
        if i + 1 < bucket_len {
            for _ in 0..extra_blank {
                out.push('\n');
            }
        }
    }
}

fn reorder_file(path: &Path) -> Result<()> {
    let src = fs::read_to_string(path).with_context(|| format!("read file {}", path.display()))?;
    let mut file: File =
        syn::parse_file(&src).with_context(|| format!("parse {}", path.display()))?;
    let line_starts = line_start_offsets(&src);

    let shebang = file.shebang.take();
    let crate_attrs = std::mem::take(&mut file.attrs);

    let (struct_enum_items, rest_items): (Vec<_>, Vec<_>) = file
        .items
        .into_iter()
        .partition(|item| matches!(item, Item::Struct(_) | Item::Enum(_) | Item::Union(_)));

    let (fn_items, other_items): (Vec<_>, Vec<_>) = rest_items
        .into_iter()
        .partition(|item| matches!(item, Item::Fn(_)));

    let sorted_struct_enums = sort_type_items_by_dependencies(struct_enum_items);

    let mut sorted_fn_items = fn_items;
    sorted_fn_items.sort_by(|a, b| {
        fn_visibility_rank(a)
            .cmp(&fn_visibility_rank(b))
            .then_with(|| fn_item_name(a).cmp(&fn_item_name(b)))
    });

    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); 14];

    let impl_items = collect_impls_and_bucket_rest(other_items, &mut buckets, &src, &line_starts);

    let type_order: Vec<String> = sorted_struct_enums
        .iter()
        .filter_map(|item| item_name(item))
        .collect();

    push_type_items(sorted_struct_enums, &mut buckets, &src, &line_starts);
    push_ordered_impls(impl_items, &type_order, &mut buckets[10]);

    for item in sorted_fn_items.into_iter() {
        let snippet = item_snippet(&item, &src, &line_starts);
        buckets[11].push(snippet);
    }

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

    let order = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 10, 11, 12];
    for idx in order {
        if let Some(bucket) = buckets.get_mut(idx) {
            write_bucket(&mut out, bucket, idx, &mut wrote_any);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_rust_file() {
        assert!(is_rust_file(Path::new("foo.rs")));
        assert!(is_rust_file(Path::new("foo.RS")));
        assert!(!is_rust_file(Path::new("foo.Rust")));
        assert!(!is_rust_file(Path::new("foo.txt")));
        assert!(!is_rust_file(Path::new("foo")));
        assert!(!is_rust_file(Path::new("foo.rs.txt")));
    }

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
