use git2::Repository;
use predicates::Predicate;

pub trait FixtureAssert {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, Repository>,
        P: Predicate<Repository>;
}

impl FixtureAssert for Repository {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, Repository>,
        P: Predicate<Repository>,
    {
        let pred = predicate.into();
        let repo = self;
        assert!(
            pred.eval(repo),
            "Repository assertion failed.\nPredicate: {}\nRepo: {:?}",
            pred,
            repo.path()
        );
        self
    }
}

pub trait IntoFixturePredicate<P, T> {
    type Predicate;
    fn into(self) -> P;
}

impl<P> IntoFixturePredicate<P, Repository> for P
where
    P: predicates::Predicate<Repository>,
{
    type Predicate = P;
    fn into(self) -> P {
        self
    }
}
