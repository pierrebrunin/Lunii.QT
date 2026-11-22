//! Outdated answers of background work, all recognized the same way.
//!
//! A request in the background (device search, story durations, podcast
//! search...) is given a [`Ticket`] of its [`Topic`], sent back with its
//! answer. A new request of the same topic makes the previous tickets
//! outdated: their answers, arriving later, are ignored.

use std::collections::HashMap;

/// What a request is about: a new request outdates the previous ones of the
/// same topic only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Topic {
    /// device search, opening, and the opened device itself: its stories
    /// read in the background belong to it
    Device,
    /// durations measured on the device (forgotten: stories changed)
    Durations,
    /// "Add a podcast" dialog: search results, and their covers
    PodcastSearch,
    /// "Add a podcast" dialog: feed checked, and its cover
    PodcastCheck,
}

/// A request of a topic, sent back with its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub topic: Topic,
    id: u32,
}

/// The current ticket of each topic.
#[derive(Default)]
pub struct Tickets(HashMap<Topic, u32>);

impl Tickets {
    /// A new request of `topic`: the previous tickets are outdated.
    pub fn issue(&mut self, topic: Topic) -> Ticket {
        let id = self.0.entry(topic).or_default();
        *id = id.wrapping_add(1);
        Ticket { topic, id: *id }
    }

    /// The current ticket of `topic`, for an answer that must not outdate
    /// the current request (a check in the background of the same state).
    pub fn current(&self, topic: Topic) -> Ticket {
        Ticket {
            topic,
            id: self.0.get(&topic).copied().unwrap_or_default(),
        }
    }

    /// The answer of `ticket` is still wanted.
    pub fn is_current(&self, ticket: Ticket) -> bool {
        self.current(ticket.topic) == ticket
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_request_outdates_the_previous_ones_of_its_topic() {
        let mut t = Tickets::default();
        let first = t.issue(Topic::Device);
        let durations = t.issue(Topic::Durations);
        assert!(t.is_current(first));
        let second = t.issue(Topic::Device);
        assert!(!t.is_current(first));
        assert!(t.is_current(second));
        assert!(t.is_current(durations), "other topic unchanged");
        assert_eq!(t.current(Topic::Device), second);
        // a topic never issued: its current ticket is valid
        assert!(t.is_current(t.current(Topic::PodcastCheck)));
    }
}
