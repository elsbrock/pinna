//! Audio handed from a run to the Brain-owned reply player.
//!
//! A sentence holds one synthesis credit until the player says it has started.
//! This bounds synthesis to the configured number of unstarted sentences.

use std::{
    collections::VecDeque,
    fmt, io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use futures_core::stream::BoxStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioKind {
    Ack,
    Sentence,
}

pub enum AudioBody {
    Stream(BoxStream<'static, io::Result<Bytes>>),
    File(PathBuf),
}

pub struct SpeechAudio {
    pub kind: AudioKind,
    pub mime: Option<String>,
    pub body: AudioBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed;

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("reply channel is closed")
    }
}

impl std::error::Error for Closed {}

/// One permit to synthesise a sentence. Sending it transfers the permit to the
/// reply source; starting that sentence releases it.
#[derive(Debug)]
pub struct Credit {
    _permit: OwnedSemaphorePermit,
}

struct Queued {
    audio: SpeechAudio,
    credit: Option<Credit>,
}

#[derive(Clone)]
pub struct ReplySink {
    tx: mpsc::Sender<Queued>,
    credits: Arc<Semaphore>,
}

pub struct ReplySource {
    rx: mpsc::Receiver<Queued>,
    credits: Arc<Semaphore>,
    unstarted: Mutex<VecDeque<Credit>>,
}

/// Create the bounded reply channel. `ahead` is the number of sentences that
/// may be synthesised but not yet have started sounding.
pub fn reply_channel(ahead: usize) -> (ReplySink, ReplySource) {
    assert!(ahead > 0, "a reply needs at least one synthesis credit");
    // The extra slot is for the one acknowledgment, which takes no credit.
    let (tx, rx) = mpsc::channel(ahead.checked_add(1).expect("reply queue capacity"));
    let credits = Arc::new(Semaphore::new(ahead));
    (
        ReplySink {
            tx,
            credits: credits.clone(),
        },
        ReplySource {
            rx,
            credits,
            unstarted: Mutex::new(VecDeque::new()),
        },
    )
}

impl ReplySink {
    /// Wait until another sentence may be synthesised.
    pub async fn credit(&self) -> Result<Credit, Closed> {
        let permit = self
            .credits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Closed)?;
        Ok(Credit { _permit: permit })
    }

    pub async fn send(&self, audio: SpeechAudio, credit: Option<Credit>) -> Result<(), Closed> {
        self.tx
            .send(Queued { audio, credit })
            .await
            .map_err(|_| Closed)
    }
}

impl ReplySource {
    pub async fn recv(&mut self) -> Option<SpeechAudio> {
        let queued = self.rx.recv().await?;
        if let Some(credit) = queued.credit {
            self.unstarted
                .lock()
                .expect("reply credit mutex")
                .push_back(credit);
        }
        Some(queued.audio)
    }

    /// Mark an item as audible. Only a sentence returns a synthesis credit.
    pub fn started(&self, kind: AudioKind) {
        if kind == AudioKind::Sentence {
            let credit = self
                .unstarted
                .lock()
                .expect("reply credit mutex")
                .pop_front();
            debug_assert!(credit.is_some(), "a started sentence holds a credit");
            drop(credit);
        }
    }
}

impl Drop for ReplySource {
    fn drop(&mut self) {
        self.credits.close();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn audio(kind: AudioKind) -> SpeechAudio {
        SpeechAudio {
            kind,
            mime: None,
            body: AudioBody::File(Path::new("unused.wav").to_path_buf()),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_third_credit_waits_for_a_sentence_to_start() {
        let (sink, mut source) = reply_channel(2);
        for _ in 0..2 {
            let credit = sink.credit().await.unwrap();
            sink.send(audio(AudioKind::Sentence), Some(credit))
                .await
                .unwrap();
        }
        sink.send(audio(AudioKind::Ack), None).await.unwrap();

        let waiting = tokio::spawn({
            let sink = sink.clone();
            async move { sink.credit().await.unwrap() }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        assert_eq!(source.recv().await.unwrap().kind, AudioKind::Sentence);
        assert_eq!(source.recv().await.unwrap().kind, AudioKind::Sentence);
        assert_eq!(source.recv().await.unwrap().kind, AudioKind::Ack);
        source.started(AudioKind::Ack);
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        source.started(AudioKind::Sentence);
        let _credit = waiting.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_the_source_wakes_a_waiting_producer() {
        let (sink, source) = reply_channel(1);
        let _credit = sink.credit().await.unwrap();
        drop(source);
        assert_eq!(sink.credit().await.unwrap_err(), Closed);
    }
}
