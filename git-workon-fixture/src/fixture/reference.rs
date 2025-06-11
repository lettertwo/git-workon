pub struct Reference<'repo> {
    reference: Option<git2::Reference<'repo>>,
}

impl<'repo> Reference<'repo> {
    pub fn new(reference: Option<git2::Reference<'repo>>) -> Self {
        Self { reference }
    }
}
