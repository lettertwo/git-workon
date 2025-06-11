use predicates::Predicate;

pub trait FixtureAssert {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P>,
        P: Predicate<git2::Repository>;
}

impl FixtureAssert for crate::fixture::Repository {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P>,
        P: Predicate<git2::Repository>,
    {
        assert(self, predicate);
        self
    }
}

pub trait IntoFixturePredicate<P>
where
    P: Predicate<git2::Repository>,
{
    type Predicate;

    fn into_repository(self) -> P;
}

impl<P> IntoFixturePredicate<P> for P
where
    P: Predicate<git2::Repository>,
{
    type Predicate = P;

    fn into_repository(self) -> P {
        self
    }
}

#[track_caller]
fn assert<P>(repo: &crate::fixture::Repository, pred: P)
where
    P: Predicate<git2::Repository>,
{
    pred.eval(repo);
}
