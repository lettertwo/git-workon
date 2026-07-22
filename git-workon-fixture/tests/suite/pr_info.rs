use git_workon_fixture::prelude::*;

#[test]
fn writes_a_single_entry() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .graphite_pr_info("feature", 42, "Add feature")
        .build()?;

    let repo = fixture.repo()?;
    let path = repo.path().join(".graphite_pr_info");
    let content = std::fs::read_to_string(&path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;

    let pr_infos = json
        .get("prInfos")
        .and_then(|v| v.as_array())
        .expect("prInfos should be an array");
    assert_eq!(pr_infos.len(), 1);
    assert_eq!(
        pr_infos[0].get("headRefName").and_then(|v| v.as_str()),
        Some("feature")
    );
    assert_eq!(pr_infos[0].get("number").and_then(|v| v.as_u64()), Some(42));
    assert_eq!(
        pr_infos[0].get("title").and_then(|v| v.as_str()),
        Some("Add feature")
    );
    assert_eq!(pr_infos[0].get("body").and_then(|v| v.as_str()), Some(""));

    Ok(())
}

#[test]
fn multiple_calls_accumulate_into_one_array() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new()
        .graphite_pr_info("feature-a", 1, "A")
        .graphite_pr_info("feature-b", 2, "B")
        .build()?;

    let repo = fixture.repo()?;
    let path = repo.path().join(".graphite_pr_info");
    let content = std::fs::read_to_string(&path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;

    let pr_infos = json
        .get("prInfos")
        .and_then(|v| v.as_array())
        .expect("prInfos should be an array");
    assert_eq!(pr_infos.len(), 2);
    assert_eq!(
        pr_infos[0].get("headRefName").and_then(|v| v.as_str()),
        Some("feature-a")
    );
    assert_eq!(
        pr_infos[1].get("headRefName").and_then(|v| v.as_str()),
        Some("feature-b")
    );

    Ok(())
}

#[test]
fn no_file_written_when_unused() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureBuilder::new().build()?;

    let repo = fixture.repo()?;
    let path = repo.path().join(".graphite_pr_info");
    assert!(
        !path.exists(),
        ".graphite_pr_info should not be created unless graphite_pr_info() is used"
    );

    Ok(())
}
