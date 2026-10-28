//! FIFO queue for confirmed filesystem operations.
//! Only confirmed jobs can enter the queue. Running jobs cannot be removed.
use std::{
    collections::VecDeque,
    io::{self, ErrorKind},
};

use crate::jobs::Work;

pub const MAX_WAITING: usize = 64;

#[derive(Default)]
pub struct WorkQueue {
    waiting: VecDeque<Work>,
}

impl WorkQueue {
    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    pub fn push(&mut self, work: Work) -> io::Result<()> {
        if self.len() >= MAX_WAITING {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("Queue full ({MAX_WAITING} waiting jobs)"),
            ));
        }
        self.waiting.push_back(work);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Work> {
        self.waiting.pop_front()
    }

    pub fn remove(&mut self, index: usize) -> Option<Work> {
        self.waiting.remove(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Work> {
        self.waiting.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::ArchiveJob;
    use std::path::PathBuf;

    fn job(name: &str) -> Work {
        Work::Archive(ArchiveJob::Unpack {
            source: PathBuf::from(format!("/tmp/{name}.zip")),
            target: PathBuf::from(format!("/tmp/{name}")),
        })
    }

    #[test]
    fn fifo_order_and_middle_removal() {
        let mut queue = WorkQueue::default();
        queue.push(job("first")).unwrap();
        queue.push(job("second")).unwrap();
        queue.push(job("third")).unwrap();
        assert_eq!(queue.len(), 3);
        assert!(queue.remove(1).unwrap().description().contains("second"));
        assert!(queue.pop().unwrap().description().contains("first"));
        assert!(queue.pop().unwrap().description().contains("third"));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn queue_limit_prevents_unbounded_growth() {
        let mut queue = WorkQueue::default();
        for _ in 0..MAX_WAITING {
            queue.push(job("waiting")).unwrap();
        }
        assert_eq!(queue.len(), MAX_WAITING);
        assert!(queue.push(job("extra")).is_err());
        assert_eq!(queue.len(), MAX_WAITING);
    }
}
