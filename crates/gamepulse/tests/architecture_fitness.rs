use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type Edge = (String, String);
type ProductionTarget = (String, String, BTreeSet<String>, BTreeSet<String>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkspaceShape {
    packages: BTreeSet<String>,
    internal_edges: BTreeSet<Edge>,
    production_targets: BTreeSet<ProductionTarget>,
}

impl WorkspaceShape {
    fn expected() -> Self {
        Self {
            packages: string_set(&[
                "gamepulse",
                "gamepulse-application",
                "gamepulse-domain",
                "gamepulse-storage-sqlite",
                "gamepulse-web",
                "gamepulse-worker-llm",
                "gamepulse-worker-media",
                "gamepulse-worker-source",
            ]),
            internal_edges: edge_set(&[
                ("gamepulse", "gamepulse-application"),
                ("gamepulse", "gamepulse-domain"),
                ("gamepulse", "gamepulse-storage-sqlite"),
                ("gamepulse", "gamepulse-web"),
                ("gamepulse", "gamepulse-worker-llm"),
                ("gamepulse", "gamepulse-worker-media"),
                ("gamepulse", "gamepulse-worker-source"),
                ("gamepulse-application", "gamepulse-domain"),
                ("gamepulse-storage-sqlite", "gamepulse-application"),
                ("gamepulse-storage-sqlite", "gamepulse-domain"),
                ("gamepulse-web", "gamepulse-application"),
                ("gamepulse-web", "gamepulse-domain"),
                ("gamepulse-worker-llm", "gamepulse-application"),
                ("gamepulse-worker-llm", "gamepulse-domain"),
                ("gamepulse-worker-media", "gamepulse-application"),
                ("gamepulse-worker-media", "gamepulse-domain"),
                ("gamepulse-worker-source", "gamepulse-application"),
                ("gamepulse-worker-source", "gamepulse-domain"),
            ]),
            production_targets: target_set(&[
                ("gamepulse", "gamepulse", &["bin"], &["bin"]),
                (
                    "gamepulse-application",
                    "gamepulse_application",
                    &["lib"],
                    &["lib"],
                ),
                ("gamepulse-domain", "gamepulse_domain", &["lib"], &["lib"]),
                (
                    "gamepulse-storage-sqlite",
                    "gamepulse_storage_sqlite",
                    &["lib"],
                    &["lib"],
                ),
                ("gamepulse-web", "gamepulse_web", &["lib"], &["lib"]),
                (
                    "gamepulse-worker-llm",
                    "gamepulse_worker_llm",
                    &["lib"],
                    &["lib"],
                ),
                (
                    "gamepulse-worker-media",
                    "gamepulse_worker_media",
                    &["lib"],
                    &["lib"],
                ),
                (
                    "gamepulse-worker-source",
                    "gamepulse_worker_source",
                    &["lib"],
                    &["lib"],
                ),
            ]),
        }
    }
}

#[test]
fn current_workspace_matches_the_architecture_contract() {
    assert_contract(&load_cargo_workspace());
}

#[test]
fn accepted_fixture_matches_the_architecture_contract() {
    assert_contract(&load_metadata_fixture("valid.json"));
}

#[test]
fn optional_worker_to_worker_dependency_is_rejected() {
    assert_rejected(
        "forbidden-worker-edge.json",
        "unexpected internal edge: gamepulse-worker-media -> gamepulse-worker-llm",
    );
}

#[test]
fn second_binary_is_rejected() {
    assert_rejected(
        "second-binary.json",
        "unexpected production target: gamepulse-worker-source:source-worker",
    );
}

#[test]
fn missing_workspace_member_is_rejected() {
    assert_rejected("missing-domain.json", "missing package: gamepulse-domain");
}

#[test]
fn extra_workspace_member_is_rejected() {
    assert_rejected(
        "extra-workspace-member.json",
        "unexpected package: gamepulse-observer",
    );
}

#[test]
fn extra_library_target_is_rejected() {
    assert_rejected(
        "extra-library-target.json",
        "unexpected production target: gamepulse-web:gamepulse_web_support",
    );
}

#[test]
fn retyped_library_target_is_rejected() {
    assert_rejected(
        "retyped-library-target.json",
        "unexpected production target: gamepulse-worker-llm:gamepulse_worker_llm",
    );
}

fn assert_rejected(fixture: &str, expected_violation: &str) {
    let violations = contract_violations(&load_metadata_fixture(fixture));
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains(expected_violation)),
        "expected {expected_violation:?}, got: {violations:#?}"
    );
}

fn assert_contract(actual: &WorkspaceShape) {
    let violations = contract_violations(actual);
    assert!(
        violations.is_empty(),
        "workspace architecture contract failed:\n{}",
        violations.join("\n")
    );
}

fn contract_violations(actual: &WorkspaceShape) -> Vec<String> {
    let expected = WorkspaceShape::expected();
    let mut violations = Vec::new();

    append_set_diff(
        &mut violations,
        "missing package",
        expected.packages.difference(&actual.packages),
    );
    append_set_diff(
        &mut violations,
        "unexpected package",
        actual.packages.difference(&expected.packages),
    );
    append_edge_diff(
        &mut violations,
        "missing internal edge",
        expected.internal_edges.difference(&actual.internal_edges),
    );
    append_edge_diff(
        &mut violations,
        "unexpected internal edge",
        actual.internal_edges.difference(&expected.internal_edges),
    );
    append_target_diff(
        &mut violations,
        "missing production target",
        expected
            .production_targets
            .difference(&actual.production_targets),
    );
    append_target_diff(
        &mut violations,
        "unexpected production target",
        actual
            .production_targets
            .difference(&expected.production_targets),
    );

    violations
}

fn append_set_diff<'a>(
    violations: &mut Vec<String>,
    label: &str,
    values: impl Iterator<Item = &'a String>,
) {
    violations.extend(values.map(|value| format!("{label}: {value}")));
}

fn append_edge_diff<'a>(
    violations: &mut Vec<String>,
    label: &str,
    values: impl Iterator<Item = &'a Edge>,
) {
    violations.extend(values.map(|(from, to)| format!("{label}: {from} -> {to}")));
}

fn append_target_diff<'a>(
    violations: &mut Vec<String>,
    label: &str,
    values: impl Iterator<Item = &'a ProductionTarget>,
) {
    violations.extend(values.map(|target| format!("{label}: {}", target_label(target))));
}

fn target_label((package, name, kinds, crate_types): &ProductionTarget) -> String {
    format!(
        "{package}:{name} [kind={}, crate_types={}]",
        kinds.iter().cloned().collect::<Vec<_>>().join(","),
        crate_types.iter().cloned().collect::<Vec<_>>().join(","),
    )
}

fn load_cargo_workspace() -> WorkspaceShape {
    let workspace_root = workspace_root();
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--manifest-path",
        ])
        .arg(workspace_root.join("Cargo.toml"))
        .output()
        .expect("cargo metadata must start");

    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must return valid JSON");
    normalize_cargo_metadata(&metadata)
}

fn load_metadata_fixture(name: &str) -> WorkspaceShape {
    normalize_cargo_metadata(&read_fixture(name))
}

fn normalize_cargo_metadata(metadata: &Value) -> WorkspaceShape {
    let member_ids = string_array(&metadata["workspace_members"]);
    let member_id_set: BTreeSet<&str> = member_ids.iter().map(String::as_str).collect();
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages must be an array");

    let mut package_names = BTreeSet::new();
    let mut names_by_manifest_directory = BTreeMap::new();
    let mut production_targets = BTreeSet::new();

    for package in packages {
        let id = string(&package["id"]);
        if !member_id_set.contains(id.as_str()) {
            continue;
        }

        let name = string(&package["name"]);
        let manifest_directory = manifest_directory(&package["manifest_path"]);
        names_by_manifest_directory.insert(manifest_directory, name.clone());
        package_names.insert(name.clone());

        for target in package["targets"]
            .as_array()
            .expect("cargo metadata targets must be an array")
        {
            let kinds = string_set_from_value(&target["kind"]);
            if is_production_target(&kinds) {
                production_targets.insert((
                    name.clone(),
                    string(&target["name"]),
                    kinds,
                    string_set_from_value(&target["crate_types"]),
                ));
            }
        }
    }

    let mut internal_edges = BTreeSet::new();
    for package in packages {
        let from_id = string(&package["id"]);
        if !member_id_set.contains(from_id.as_str()) {
            continue;
        }

        let from_name = string(&package["name"]);
        for dependency in package["dependencies"]
            .as_array()
            .expect("cargo metadata package dependencies must be an array")
        {
            let Some(path) = dependency["path"].as_str() else {
                continue;
            };
            let Some(to_name) = names_by_manifest_directory.get(Path::new(path)) else {
                continue;
            };

            internal_edges.insert((from_name.clone(), to_name.clone()));
        }
    }

    WorkspaceShape {
        packages: package_names,
        internal_edges,
        production_targets,
    }
}

fn is_production_target(kinds: &BTreeSet<String>) -> bool {
    [
        "bin",
        "lib",
        "cdylib",
        "dylib",
        "proc-macro",
        "rlib",
        "staticlib",
    ]
    .iter()
    .any(|kind| kinds.contains(*kind))
}

fn manifest_directory(manifest_path: &Value) -> PathBuf {
    Path::new(&string(manifest_path))
        .parent()
        .expect("package manifest path must have a parent directory")
        .to_path_buf()
}

fn read_fixture(name: &str) -> Value {
    let path = fixture_dir().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|error| {
        panic!("failed to read fixture {}: {error}", path.display());
    });
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!("invalid fixture {}: {error}", path.display());
    })
}

fn string(value: &Value) -> String {
    value.as_str().expect("expected a JSON string").to_owned()
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| items.iter().map(string).collect())
        .unwrap_or_default()
}

fn string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn string_set_from_value(value: &Value) -> BTreeSet<String> {
    string_array(value).into_iter().collect()
}

fn edge_set(values: &[(&str, &str)]) -> BTreeSet<Edge> {
    values
        .iter()
        .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
        .collect()
}

fn target_set(values: &[(&str, &str, &[&str], &[&str])]) -> BTreeSet<ProductionTarget> {
    values
        .iter()
        .map(|(package, name, kinds, crate_types)| {
            (
                (*package).to_owned(),
                (*name).to_owned(),
                string_set(kinds),
                string_set(crate_types),
            )
        })
        .collect()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture")
}
