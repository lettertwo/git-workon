use git2::Repository;
use predicates::Predicate;

pub trait FixtureAssert {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, Repository>,
        P: Predicate<Repository> + 'static;
}

pub trait DescribesActual {
    fn describe_actual(&self, repo: &git2::Repository) -> Option<String>;
}

impl FixtureAssert for Repository {
    #[track_caller]
    fn assert<I, P>(&self, predicate: I) -> &Self
    where
        I: IntoFixturePredicate<P, Repository>,
        P: Predicate<Repository> + 'static,
    {
        let pred = predicate.into();
        let repo = self;

        let extra = match (&pred as &dyn std::any::Any).downcast_ref::<&dyn DescribesActual>() {
            Some(describer) => describer.describe_actual(repo),
            None => None,
        };

        if let Some(extra) = extra {
            assert!(
                pred.eval(repo),
                "Repository assertion failed.\nPredicate: {}\nActual: {}\nRepo: {:?}",
                pred,
                extra,
                repo.path(),
            );
        } else {
            assert!(
                pred.eval(repo),
                "Repository assertion failed.\nPredicate: {}\nRepo: {:?}",
                pred,
                repo.path(),
            );
        }

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
