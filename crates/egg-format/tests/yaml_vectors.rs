//! Reference vectors produced by the original panel's Symfony YAML
//! (`Yaml::dump($v, 10, 2, DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP)`).

#[test]
fn matches_symfony_yaml() {
    let data: serde_json::Value = serde_json::from_str(include_str!("yaml_vectors.json")).unwrap();
    let mut failures = Vec::new();
    for (i, v) in data.as_array().unwrap().iter().enumerate() {
        let want = v["yaml"].as_str().unwrap();
        let got = egg_format::yaml::dump(&v["input"]);
        if got != want {
            failures.push(format!("#{i} input={}\n  want: {want:?}\n  got:  {got:?}", v["input"]));
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}
