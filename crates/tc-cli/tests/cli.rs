use std::fs;
use std::process::Command;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
    for (p, size) in [
        ("a.txt", 100),
        ("b.bin", 2000),
        ("sub/c", 30),
        ("sub/deeper/d", 4),
    ] {
        fs::File::create(dir.path().join(p))
            .unwrap()
            .set_len(size)
            .unwrap();
    }
    dir
}

fn treecraft(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_treecraft"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn json_output_has_totals_and_children() {
    let dir = fixture();
    for scanner in ["auto", "portable"] {
        let out = treecraft(&[
            "scan",
            dir.path().to_str().unwrap(),
            "--json",
            "--depth",
            "2",
            "--scanner",
            scanner,
        ]);
        assert!(out.status.success(), "{out:?}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["size"], 2134);
        assert_eq!(v["files"], 4);
        assert_eq!(v["dirs"], 2);
        let names: Vec<_> = v["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["b.bin", "a.txt", "sub"]);
        let sub = &v["children"][2];
        assert_eq!(sub["size"], 34);
        // depth 2 stops below sub/deeper
        assert!(
            sub["children"][0]["children"].is_null() || sub["children"][1]["children"].is_null()
        );
    }
}

#[test]
fn text_output() {
    let dir = fixture();
    let out = treecraft(&["scan", dir.path().to_str().unwrap(), "--top", "2"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("2.08 KB"), "{text}");
    assert!(text.contains("b.bin"), "{text}");
    assert!(text.contains("(1 more items)"), "{text}");
    assert!(!text.contains("sub"), "top 2 should hide sub: {text}");
}

#[test]
fn missing_root_fails() {
    let dir = tempfile::tempdir().unwrap();
    let out = treecraft(&["scan", dir.path().join("nope").to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot scan"));
}
