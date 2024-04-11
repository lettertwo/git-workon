use std::path::Path;

use git2::Repository;
use miette::Result;

pub fn workon_root(repo: &Repository) -> Result<&Path> {
    let path = repo.path();

    match repo.workdir() {
        Some(workdir) if workdir != path => {
            let repo_ancestors: Vec<_> = path.ancestors().collect();
            let common_root = workdir
                .ancestors()
                .find(|ancestor| repo_ancestors.contains(ancestor));
            if let Some(common_root) = common_root {
                return Ok(common_root);
            }
        }
        _ => {}
    }

    Ok(path.parent().unwrap())
}
