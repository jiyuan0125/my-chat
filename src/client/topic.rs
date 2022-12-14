use std::str::FromStr;

/// Built topic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Topic(String);

impl Topic {
    /// Returns the id of the topic.
    #[inline]
    pub fn id(&self) -> &str {
        &self.0
    }

    pub fn new<S>(name: S) -> Topic
        where
            S: Into<String>,
    {
        Topic(name.into())
    }
}

impl From<Topic> for String {
    fn from(topic: Topic) -> String {
        topic.0
    }
}

impl FromStr for Topic {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Topic::new(s))
    }
}
