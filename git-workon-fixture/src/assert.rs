use predicates::Predicate;

pub trait FixtureAssert {
    type Target;

    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, Self::Target>,
        P: Predicate<Self::Target>;
}

impl FixtureAssert for crate::fixture::Repository {
    type Target = git2::Repository;

    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, git2::Repository>,
        P: Predicate<git2::Repository>,
    {
        let pred = predicate.into();
        let repo = self.unwrap();
        assert!(
            pred.eval(repo),
            "Repository assertion failed.\nPredicate: {}\nRepo: {:?}",
            pred,
            repo.path()
        );
        self
    }
}

impl<'repo> FixtureAssert for crate::fixture::Reference<'repo> {
    type Target = git2::Reference<'repo>;

    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, git2::Reference<'repo>>,
        P: Predicate<git2::Reference<'repo>>,
    {
        let pred = predicate.into();
        let reference = self.unwrap().expect("Reference does not exist");
        assert!(
            pred.eval(reference),
            "Reference assertion failed.\nPredicate: {}\nReference: {:?}",
            pred,
            reference.name()
        );
        self
    }
}

impl<'repo> FixtureAssert for crate::fixture::Branch<'repo> {
    type Target = git2::Branch<'repo>;

    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, git2::Branch<'repo>>,
        P: Predicate<git2::Branch<'repo>>,
    {
        let pred = predicate.into();
        let branch = self.unwrap().expect("Branch does not exist");
        assert!(
            pred.eval(branch),
            "Branch assertion failed.\nPredicate: {}\nBranch: {:?}",
            pred,
            branch.name()
        );
        self
    }
}

pub trait IntoFixturePredicate<P, T> {
    type Predicate;
    fn into(self) -> P;
}

impl<P> IntoFixturePredicate<P, git2::Repository> for P
where
    P: predicates::Predicate<git2::Repository>,
{
    type Predicate = P;
    fn into(self) -> P {
        self
    }
}

impl<'repo, P> IntoFixturePredicate<P, git2::Reference<'repo>> for P
where
    P: predicates::Predicate<git2::Reference<'repo>>,
{
    type Predicate = P;
    fn into(self) -> P {
        self
    }
}

impl<'repo, P> IntoFixturePredicate<P, git2::Branch<'repo>> for P
where
    P: predicates::Predicate<git2::Branch<'repo>>,
{
    type Predicate = P;
    fn into(self) -> P {
        self
    }
}
