use super::{Branch, Reference};

pub struct Repository {
    repo: git2::Repository,
}

impl Repository {
    pub fn new(repo: git2::Repository) -> Self {
        Self { repo }
    }

    pub fn head(&self) -> Reference {
        Reference::new(self.repo.head().ok())
    }

    pub fn branch(&self, name: &str) -> Branch {
        Branch::new(self.repo.find_branch(name, git2::BranchType::Local).ok())
    }

    // pub fn has_branch(&self, branch: &str) -> bool {
    //     self.repo
    //         .find_branch(branch, git2::BranchType::Local)
    //         .is_ok()
    // }
    //
    // pub fn has_main_branch(&self) -> bool {
    //     self.has_branch("main")
    // }
}
