pub struct Branch<'repo> {
    branch: Option<git2::Branch<'repo>>,
}

impl<'repo> Branch<'repo> {
    pub fn new(branch: Option<git2::Branch<'repo>>) -> Self {
        Self { branch }
    }
}
