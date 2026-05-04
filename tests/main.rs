use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cargo_bin() -> std::path::PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("cargo-refmt")
}

fn run_reorder(path: &Path) -> String {
    let bin_path = cargo_bin();
    let output = Command::new(&bin_path)
        .arg(path)
        .output()
        .unwrap_or_else(|e| panic!("failed to run reorder at {:?}: {}", bin_path, e));
    assert!(
        output.status.success(),
        "reorder failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(path).expect("failed to read file")
}

#[test]
fn test_bare_mod_tests_not_at_bottom() {
    let path = test_dir().join("bare_mod_tests.rs");
    fs::write(
        &path,
        "\
mod tests;

use std::fs;

pub fn run() {}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    let use_pos = result.find("use std::fs").expect("use not found");
    let mod_pos = result.find("mod tests").expect("mod tests not found");
    let fn_pos = result.find("pub fn run").expect("fn not found");
    assert!(
        use_pos < mod_pos,
        "use should come before mod tests: got use at {use_pos}, mod at {mod_pos}"
    );
    assert!(
        mod_pos < fn_pos,
        "bare mod tests should come before fn: got mod at {mod_pos}, fn at {fn_pos}"
    );
}

#[test]
fn test_cfg_test_module_at_bottom() {
    let path = test_dir().join("cfg_test_module.rs");
    fs::write(
        &path,
        "\
#[cfg(test)]
mod tests {
    use super::*;
}

use std::fs;

pub fn run() {}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    let use_pos = result.find("use std::fs").expect("use not found");
    let fn_pos = result.find("pub fn run").expect("fn not found");
    let test_pos = result.find("#[cfg(test)]").expect("#[cfg(test)] not found");
    assert!(
        test_pos > use_pos,
        "#[cfg(test)] mod should be after use at {use_pos}, got test at {test_pos}"
    );
    assert!(
        test_pos > fn_pos,
        "#[cfg(test)] mod should be after fn at {fn_pos}, got test at {test_pos}"
    );
}

#[test]
fn test_constants_no_blank_lines() {
    let path = test_dir().join("constants.rs");
    fs::write(
        &path,
        "\
const DEFAULT_MODEL: &str = \"gpt-5.4\";

const EXECUTOR_TURNS: usize = 12;

const IMPLEMENTATION_RETRY_LIMIT: usize = 3;

const MAX_FINAL_REVIEW_PASSES: usize = 3;

const WORKFLOW_MAX_CONCURRENCY: usize = 4;

const WORKTREE_DIR: &str = \".mmat-worktrees\";
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
const DEFAULT_MODEL: &str = \"gpt-5.4\";
const EXECUTOR_TURNS: usize = 12;
const IMPLEMENTATION_RETRY_LIMIT: usize = 3;
const MAX_FINAL_REVIEW_PASSES: usize = 3;
const WORKFLOW_MAX_CONCURRENCY: usize = 4;
const WORKTREE_DIR: &str = \".mmat-worktrees\";
"
    );
}

fn test_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/regression");
    fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

#[test]
fn test_fn_visibility_order() {
    let path = test_dir().join("fn_visibility.rs");
    fs::write(
        &path,
        "\
fn private_fn() {}

pub(crate) fn crate_fn() {}

pub fn public_fn() {}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    let pub_pos = result.find("pub fn public_fn").expect("pub fn not found");
    let crate_pos = result
        .find("pub(crate) fn crate_fn")
        .expect("pub(crate) fn not found");
    let priv_pos = result.find("fn private_fn").expect("private fn not found");
    assert!(
        pub_pos < crate_pos,
        "pub fn should come before pub(crate) fn: got pub at {pub_pos}, pub(crate) at {crate_pos}"
    );
    assert!(
        crate_pos < priv_pos,
        "pub(crate) fn should come before private fn: got pub(crate) at {crate_pos}, private at {priv_pos}"
    );
}

#[test]
fn test_guest_context_before_shared_memory_handle() {
    let path = test_dir().join("guest_context_dependencies.rs");
    fs::write(
        &path,
        "\
pub struct SharedMemoryHandle {
    context: GuestContext,
    descriptor: SharedMappingDescriptor,
    owns_region: bool,
}

#[derive(Clone)]
pub struct GuestContext {
    host: Arc<dyn GuestHost>,
    scope_context: ScopeContext,
}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    let guest_context_pos = result
        .find("pub struct GuestContext")
        .expect("GuestContext not found");
    let shared_memory_pos = result
        .find("pub struct SharedMemoryHandle")
        .expect("SharedMemoryHandle not found");

    assert!(
        guest_context_pos < shared_memory_pos,
        "GuestContext should come before SharedMemoryHandle: GuestContext at {}, SharedMemoryHandle at {}",
        guest_context_pos,
        shared_memory_pos
    );
}

#[test]
fn test_impl_order_by_type_order() {
    let path = test_dir().join("impl_order.rs");
    fs::write(
        &path,
        "\
trait ArtifactLookup {}

pub struct ArtifactId(pub String);

pub struct TransitionId(pub String);

pub struct ArtifactRef {
    data: i32,
}

impl ArtifactId {
    pub fn new() -> Self {
        Self(String::new())
    }
}

impl ArtifactRef {
    pub fn downcast_ref(&self) -> i32 {
        self.data
    }
}

impl ArtifactLookup for ArtifactId {}

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionId {
    pub fn new(id: String) -> Self {
        Self(id)
    }
}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
trait ArtifactLookup {}

pub struct ArtifactId(pub String);

pub struct TransitionId(pub String);

pub struct ArtifactRef {
    data: i32,
}

impl ArtifactId {
    pub fn new() -> Self {
        Self(String::new())
    }
}

impl ArtifactLookup for ArtifactId {}

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionId {
    pub fn new(id: String) -> Self {
        Self(id)
    }
}

impl ArtifactRef {
    pub fn downcast_ref(&self) -> i32 {
        self.data
    }
}
"
    );
}

#[test]
fn test_impl_order_with_generics_paths_and_unknown_targets() {
    let path = test_dir().join("impl_order_generics.rs");
    fs::write(
        &path,
        "\
trait Display {}

trait LocalTrait {}

struct Local;

struct Generic<T> {
    value: T,
}

impl<T> std::fmt::Display for Generic<T>
where
    T: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(formatter)
    }
}

impl external::Local {
    fn external() {}
}

impl<T> LocalTrait for Generic<T> {}

impl<T> Generic<T> {
    fn value(&self) -> &T {
        &self.value
    }
}

impl<T> Generic<T> {
    fn into_value(self) -> T {
        self.value
    }
}

impl crate::Local {
    fn new() -> Self {
        Self
    }
}

impl Default for Generic<u8> {
    fn default() -> Self {
        Self { value: 0 }
    }
}

impl<T> Display for Generic<T> {}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
trait Display {}

trait LocalTrait {}

struct Local;

struct Generic<T> {
    value: T,
}

impl crate::Local {
    fn new() -> Self {
        Self
    }
}

impl<T> Generic<T> {
    fn value(&self) -> &T {
        &self.value
    }
}

impl<T> Generic<T> {
    fn into_value(self) -> T {
        self.value
    }
}

impl<T> LocalTrait for Generic<T> {}

impl<T> Display for Generic<T> {}

impl<T> std::fmt::Display for Generic<T>
where
    T: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(formatter)
    }
}

impl Default for Generic<u8> {
    fn default() -> Self {
        Self { value: 0 }
    }
}

impl external::Local {
    fn external() {}
}
"
    );
}

#[test]
fn test_import_ordering() {
    let path = test_dir().join("imports.rs");
    fs::write(
        &path,
        "\
use uuid::Uuid;
use std::fs::File;
use crate::module::Blah;
use serde::Deserialize;
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
use std::fs::File;

use serde::Deserialize;
use uuid::Uuid;

use crate::module::Blah;
"
    );
}

#[test]
fn test_mod_after_use_not_at_bottom() {
    let path = test_dir().join("mod_after_use.rs");
    fs::write(
        &path,
        "\
mod context;
mod ids;

use std::fs;
use std::path::Path;

pub fn run() {}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    let use_pos = result.find("use std::fs").expect("use statement not found");
    let mod_pos = result.find("mod context").expect("mod not found");
    let fn_pos = result.find("pub fn run").expect("fn not found");
    assert!(
        use_pos < mod_pos,
        "use statements should come before mod: got use at {use_pos}, mod at {mod_pos}"
    );
    assert!(
        mod_pos < fn_pos,
        "mod should come before fn: got mod at {mod_pos}, fn at {fn_pos}"
    );
}

#[test]
fn test_modules_no_blank_lines_between() {
    let path = test_dir().join("modules.rs");
    fs::write(
        &path,
        "\
pub mod context;

pub mod ids;

pub mod journal;

pub mod run;
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
pub mod context;
pub mod ids;
pub mod journal;
pub mod run;
"
    );
}

#[test]
fn test_no_extra_blank_line_after_last_item() {
    let path = test_dir().join("last_item.rs");
    fs::write(
        &path,
        "\
use uuid::Uuid;

pub type RunId = Uuid;

pub struct Foo {
    bar: i32,
}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert!(
        !result.ends_with("\n\n\n"),
        "should not have extra blank line after last item"
    );
}

#[test]
fn test_preserve_no_trailing_newline() {
    let path = test_dir().join("no_newline.rs");
    fs::write(
        &path,
        "\
use uuid::Uuid;

pub type RunId = Uuid;",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert!(
        !result.ends_with('\n'),
        "should not add trailing newline to file without one"
    );
}

#[test]
fn test_preserve_trailing_newline() {
    let path = test_dir().join("with_newline.rs");
    fs::write(
        &path,
        "\
use uuid::Uuid;

pub type RunId = Uuid;
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert!(result.ends_with('\n'), "should preserve trailing newline");
    assert!(
        !result.ends_with("\n\n"),
        "should not add extra trailing newline"
    );
}

#[test]
fn test_type_aliases_no_extra_blank_lines() {
    let path = test_dir().join("types.rs");
    fs::write(
        &path,
        "\
use uuid::Uuid;

pub type RunId = Uuid;
pub type ArtifactId = Uuid;
pub type TransitionId = &'static str;
pub type ValidatorId = &'static str;
pub type ExecutorId = &'static str;
pub type FindingId = Uuid;
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
use uuid::Uuid;

pub type ArtifactId = Uuid;
pub type ExecutorId = &'static str;
pub type FindingId = Uuid;
pub type RunId = Uuid;
pub type TransitionId = &'static str;
pub type ValidatorId = &'static str;
"
    );
}

#[test]
fn test_type_order_dependency_before_dependent() {
    let path = test_dir().join("sort_by_usage.rs");
    fs::write(
        &path,
        "\
enum Foo {
    Opt(Bar),
}

struct Bar;
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    assert_eq!(
        result,
        "\
struct Bar;

enum Foo {
    Opt(Bar),
}
"
    );
}

#[test]
fn test_type_order_preserves_mixed_visibility_source_order() {
    let path = test_dir().join("private_structs.rs");
    fs::write(
        &path,
        "\
struct PrivateStruct {
    y: i32,
}

#[derive(Clone)]
pub struct PublicStruct {
    x: i32,
}

struct PrivateEnum {
    x: i32,
}

pub enum PublicEnum {
    A,
    B,
}
",
    )
    .expect("failed to write test file");

    let result = run_reorder(&path);

    let public_struct_pos = result
        .find("pub struct PublicStruct")
        .expect("public struct not found");
    let private_struct_pos = result
        .find("struct PrivateStruct")
        .expect("private struct not found");
    let public_enum_pos = result
        .find("pub enum PublicEnum")
        .expect("public enum not found");
    let private_enum_pos = result
        .find("struct PrivateEnum")
        .expect("private enum not found");

    assert!(
        private_struct_pos < public_struct_pos,
        "private struct should keep source order before public struct: private at {}, public at {}",
        private_struct_pos,
        public_struct_pos,
    );
    assert!(
        private_enum_pos < public_enum_pos,
        "private enum should keep source order before public enum: private at {}, public at {}",
        private_enum_pos,
        public_enum_pos
    );

    assert!(
        public_struct_pos < private_enum_pos,
        "public struct should keep source order before private enum: public struct at {}, private enum at {}",
        public_struct_pos,
        private_enum_pos
    );
}
