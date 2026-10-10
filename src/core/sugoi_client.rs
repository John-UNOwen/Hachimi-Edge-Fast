use std::sync::{Arc, Mutex, MutexGuard, mpsc::{self, Receiver, Sender}};
use std::sync::atomic::{AtomicUsize, Ordering};

use fnv::{FnvHashMap, FnvHashSet};
use once_cell::sync::Lazy;
use serde::Serialize;

use crate::il2cpp::{symbols::GCHandle,types::Il2CppObject};

use super::{Error, Hachimi};

pub struct SugoiClient {
    agent: ureq::Agent,
    url: String,
    request_lock: Mutex<()>,
}

/// A component the translation pass has seen the game give a string to: the string it was given,
/// and a **weak** handle back to the component. Weak on purpose - the handle never keeps a game
/// component alive, it is only the one thing that can answer whether the component is still there.
pub struct StringInfo {
    pub str_handle: GCHandle,
    pub str: String
}

/// What an entry in a tracked-component registry has to answer for the pass that writes through it.
pub trait TrackedComponent {
    /// The original string this entry was recorded for.
    fn original(&self) -> &str;

    /// The component this entry points at *now*: the handle's target, or null once the game has
    /// let it go.
    ///
    /// This is the only answer either question has. A pass that wants a component it may call has
    /// to take it from here, at the call - an address copied out of the registry earlier is a
    /// snapshot of a moment that has already passed (C11).
    fn live_target(&self) -> *mut Il2CppObject;
}

impl TrackedComponent for StringInfo {
    fn original(&self) -> &str {
        &self.str
    }

    fn live_target(&self) -> *mut Il2CppObject {
        self.str_handle.target()
    }
}

/// Every registry here is read from detours, so a lock an earlier panic poisoned has to answer
/// rather than panic inside an `extern "C"` frame (C2, AGENTS section 6).
fn shared_registry<'a, E>(cell: &'a Mutex<FnvHashMap<usize, E>>) -> MutexGuard<'a, FnvHashMap<usize, E>> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a key answers when it is looked up again at the write (C11).
enum KeyAnswer {
    /// Nothing live sits under this key any more: the entry was dropped, or its handle stopped
    /// answering for the component. Either way there is no component to call.
    Gone,
    /// Something live sits under this key, and it is not the entry this translation was for (C8).
    Rekeyed,
    /// The entry this translation was for, and the component that entry's own handle answers for
    /// right now. This is the only variant a write is allowed to use.
    Live(*mut Il2CppObject),
}

/// Look one key up again, and answer for the component sitting under it *now*.
///
/// The lookup, the identity question and the null question all happen while the registry lock is
/// held, on the value the lookup produced: the identity is asked of the entry that was just found,
/// and the null question is asked of that same entry's handle. Nothing leaves here as a copy to be
/// tested afterwards, which is the half the retired shape got wrong - it tested a weak handle and
/// then called with the map key.
#[inline]
fn answer_key<E: TrackedComponent>(
    tracker: &Mutex<FnvHashMap<usize, E>>,
    key: usize,
    original: &str,
) -> KeyAnswer {
    let registry = shared_registry(tracker);

    let Some(entry) = registry.get(&key) else { return KeyAnswer::Gone };

    if entry.original() != original {
        return KeyAnswer::Rekeyed;
    }

    let target = entry.live_target();
    if target.is_null() { KeyAnswer::Gone } else { KeyAnswer::Live(target) }
}

/// What one write answered for the live entry the pass handed its caller (C11).
///
/// The reason travels with the answer instead of being guessed by the pass, because a decline has
/// more than one reason in the shipped callers: `Text.rs` and `TextMesh.rs` stop on a guarded
/// trampoline that answered None, and they also stop when the translated string's own handle answers
/// null. One count carrying one of those labels describes a take-down that never happened whenever
/// the other one is what actually happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAnswer {
    /// The caller completed the call on the component it was handed, with a value in hand. The only
    /// answer that is a translation landing.
    Written,
    /// Live entry, but this caller's own hook had no callable original behind it (C1): no trampoline
    /// in the registry, nothing waiting in the take-down queue and no opened target behind the
    /// wrapper. This is where a skipped arming, a take-down the backend refused, and a hook
    /// `disabled_hooks` switched off all leave the caller.
    NoCallableOriginal,
    /// Live entry, and the callable original was in hand, but the value this pass prepared for it
    /// answered null, so there was no string to hand over. Nothing to do with this hook's arming:
    /// the handle that stopped answering is the translated string's strong handle made by `prepare`,
    /// while the component's weak handle was still answering for the component.
    NoValueInHand,
}

/// What one pass through [`apply_translation_pass`] wrote and what it refused, counted by the pass
/// itself.
///
/// There are five ways a key can end, and each is decided by something that happened to that key, or
/// by the answer its caller gave at the write, not by the pass having reached it. `gone` is a
/// component the game retired under an earlier write of the same pass, `rekeyed` a key the game
/// handed to other text since the scan, `no_callable_original` a live entry whose caller had no
/// callable original behind its own hook, `no_value_in_hand` a live entry whose caller had the hook
/// in hand and no string to give it, and `written` a component the caller said it called through.
/// The two refusals stay in counts of their own because they are facts about different things, one
/// about this hook's arming and one about this pass's string, and a run that has to name why a pass
/// refused reads the count that moved rather than a sum labelled with a guess.
/// `keys == written + gone + rekeyed + no_callable_original + no_value_in_hand` is the statement that
/// every key this pass looked at was either called through or refused - never called through on the
/// strength of a copy, and never counted as a call the caller did not make.
#[derive(Debug, Default)]
pub struct PassTally {
    /// Finished translations this pass took.
    pub translations: usize,
    /// Registry keys it looked up.
    pub keys: usize,
    /// Components the caller reported calling through, on a live entry, with a value in hand. This
    /// is the only count a run may read as translations landing: a non-zero `written` is a
    /// `set_text` the shipped caller completed, not a key the pass happened to reach.
    pub written: usize,
    /// Keys that answered "no component" at the lookup.
    pub gone: usize,
    /// Keys that no longer held the entry this translation was for (C8).
    pub rekeyed: usize,
    /// Live entries the caller declined to write because it had no callable original behind its own
    /// hook (C11): no trampoline in the registry, nothing waiting in the take-down queue and no
    /// opened target behind the wrapper, which is where a target that resolved to nothing leaves a
    /// skipped arming and where a take-down the backend refused leaves a completed one. Nothing
    /// reached the game for these keys, so they are not in `written`. Counting them with the writes
    /// is what let the apply line report a landing the game never received.
    pub no_callable_original: usize,
    /// Live entries whose caller had the callable original and declined because the value this pass
    /// prepared answered null (C11): the translated string never became a game string, so there was
    /// nothing to write and nothing reached the game. This is not a take-down and not a skipped
    /// arming, and it must not be read as one: a run that blamed the hook for this count would go
    /// looking for a `set_text_hook` take-down the backend never refused.
    pub no_value_in_hand: usize,
}

impl PassTally {
    /// Route one write answer into the count for the reason the caller actually gave (C11). The pass
    /// used to fold every decline into one bucket and label that bucket with one cause; the answer
    /// the caller returns at the write is what decides the bucket now.
    #[inline]
    pub fn record(&mut self, answer: WriteAnswer) {
        match answer {
            WriteAnswer::Written => self.written += 1,
            WriteAnswer::NoCallableOriginal => self.no_callable_original += 1,
            WriteAnswer::NoValueInHand => self.no_value_in_hand += 1,
        }
    }

    /// Keys this pass reached live and did not call through, whatever the reason. Arithmetic only:
    /// which reason a run has to chase comes from the two counts, never from this sum.
    #[inline]
    pub fn refused(&self) -> usize {
        self.no_callable_original + self.no_value_in_hand
    }

    /// Every key the pass looked up lands in exactly one of the five counts, and no key lands in two.
    /// A sixth outcome, a key neither called through nor refused, is the check then use shape coming
    /// back, and a refusal tallied as a write is a count claiming a call the caller never made.
    #[inline]
    pub fn accounted(&self) -> usize {
        self.written + self.gone + self.rekeyed + self.no_callable_original + self.no_value_in_hand
    }
}

// Which passes get a line: the fork's pattern (AGENTS section 6 - counts and periodic totals, not a
// line per call), in the same shape as `AnimationSpeed`'s apply line. A session finishes a
// translation every time the game reaches text it has not seen, so the detail window covers the
// first screens and the chunk is what keeps a long session from writing a line per string.
const PASS_DETAIL_LIMIT: usize = 6;
const PASS_CHUNK: usize = 64;

static APPLY_PASSES: AtomicUsize = AtomicUsize::new(0);
static APPLY_TRANSLATIONS: AtomicUsize = AtomicUsize::new(0);
static APPLY_KEYS: AtomicUsize = AtomicUsize::new(0);
static APPLY_WRITES: AtomicUsize = AtomicUsize::new(0);
static APPLY_GONE: AtomicUsize = AtomicUsize::new(0);
static APPLY_REKEYED: AtomicUsize = AtomicUsize::new(0);
static APPLY_NO_CALLABLE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static APPLY_NO_VALUE_IN_HAND: AtomicUsize = AtomicUsize::new(0);

fn pass_is_worth_logging(pass: usize) -> bool {
    pass <= PASS_DETAIL_LIMIT || pass % PASS_CHUNK == 0
}

/// The line a run reads to see this pass reach the screen, and what it refused on the way (C11).
/// Cumulative over both registries - `Text` and `TextMesh` are one feature - and named for the
/// component the pass just ran for. `info!` and not `debug!` on purpose: the run this item still
/// owes is "translations land on screen", and that has to be readable from an ordinary session with
/// `enable_file_logging` alone, not only from a `debug_mode` probe run. Only the passes
/// `pass_is_worth_logging` picks pay for the formatting; the accounting itself is seven atomic adds
/// per pass, on a path that runs once per finished translation and not once per call to `set_text`.
///
/// The last two counts are what keeps the third honest. `written` is the number of calls the callers
/// completed; a pass that reached live keys and refused them shows up here as 0 written and a rising
/// count in one of the last two, and which one rises is the reason the line names: the sixth says
/// this wrapper had no callable original behind it, the seventh says the string this pass prepared
/// never reached the game as a value. A non-zero count in either one means the pass is refusing live
/// writes and this item stays open until the line shows which of the two it is, because the two have
/// different causes and different fixes: the sixth is a hook this build did not arm, the seventh is a
/// translated string the game did not take. A hook the config switched off never runs, so its line
/// stands at 0 keys looked up and 0 written. Neither refusal reads as a landing, and neither is
/// evidence about the other.
fn report_pass(site: &str, pass: usize) {
    if pass_is_worth_logging(pass) {
        info!(
            "{site} translation apply pass {pass}: {} translations, {} keys looked up, {} written, \
             {} components gone at the lookup, {} keys holding other text, {} live entries with no \
             callable original behind the hook, {} live entries with no translated string in hand",
            APPLY_TRANSLATIONS.load(Ordering::Relaxed),
            APPLY_KEYS.load(Ordering::Relaxed),
            APPLY_WRITES.load(Ordering::Relaxed),
            APPLY_GONE.load(Ordering::Relaxed),
            APPLY_REKEYED.load(Ordering::Relaxed),
            APPLY_NO_CALLABLE_ORIGINAL.load(Ordering::Relaxed),
            APPLY_NO_VALUE_IN_HAND.load(Ordering::Relaxed),
        );
    }
}

/// Push finished translations into every tracked component that still holds the string they were
/// translated from (C11).
///
/// The pass takes **keys** out of the registry, never targets: a key is the identity an entry is
/// stored under, not a value to call with. For each key it goes back into the registry, takes the
/// entry still sitting under that key, asks that entry for its live target, and calls with what it
/// answered - so the pointer used is the pointer that was just read, and it was read at the moment
/// of the call, with nothing in between that could have run the component away. `answer_key` is
/// that lookup, and it asks its questions of the value it read rather than of a copy of it.
///
/// The shape this replaces built a `Vec<(address, string)>` under the lock and walked it after the
/// lock was gone. That checks one thing and uses another: the liveness test read the weak handle,
/// the call used the map key, and between them every earlier write in the list has already run game
/// code - the original `set_text`, which can close a view, reload a scene or recycle a pooled
/// element - on a path where the game is allowed to destroy the components later in the list. Their
/// addresses were still in the Vec.
///
/// `prepare` turns one finished translation into whatever `write` needs and runs once per
/// translation, not once per component. Both are parameters so that the pass a shipped hook runs is
/// the pass a unit test drives: AGENTS section 4, a test proves a decision and never a cost.
///
/// `write` answers with a [`WriteAnswer`], naming the reason a decline happened. Only the caller can
/// name it: the address it needs is its own hook's trampoline, which lives in the wrapper's cache and
/// not here (C1), and the string it writes is the value this pass prepared, which only the caller can
/// see at the write. Before, a caller that declined had no way to say so: both shipped callers reached
/// `let Some(set_text) = get_orig_fn_guarded!(..) else { return }` and returned, and the pass still
/// added the key to `written`, so a component whose wrapper answers with no trampoline, no queued
/// take-down and no callable target behind it was reported as called through, and the invariant this
/// item runs on claimed it had been. The reason is part of the answer for the same reason the pass
/// re-derives the component per write: the pass may only claim what its caller actually did, and only
/// the caller can say whether what it did not do was having no hook to call through or having no
/// string to hand over.
pub fn apply_translation_pass<E, V, P, W>(
    site: &'static str,
    tracker: &Mutex<FnvHashMap<usize, E>>,
    completed: &[(String, String)],
    prepare: P,
    mut write: W,
) -> PassTally
where
    E: TrackedComponent,
    P: Fn(&str) -> V,
    W: FnMut(*mut Il2CppObject, &V) -> WriteAnswer,
{
    let mut tally = PassTally { translations: completed.len(), ..PassTally::default() };
    let mut keys: Vec<usize> = Vec::new();

    {
        // Housekeeping, not a guarantee: entries whose component is gone are dropped so the map
        // cannot grow without bound. Nothing downstream leans on this having run - a write re-reads
        // its own entry and skips a dead one by itself.
        let mut registry = shared_registry(tracker);
        registry.retain(|_, entry| !entry.live_target().is_null());
    }

    for (original, translated) in completed {
        let value = prepare(translated);

        keys.clear();
        {
            let registry = shared_registry(tracker);
            keys.extend(
                registry.iter()
                    .filter(|(_, entry)| entry.original() == original.as_str())
                    .map(|(key, _)| *key)
            );
        }

        tally.keys += keys.len();

        for key in &keys {
            let target = match answer_key(tracker, *key, original.as_str()) {
                KeyAnswer::Live(target) => target,
                KeyAnswer::Gone => { tally.gone += 1; continue; }
                KeyAnswer::Rekeyed => { tally.rekeyed += 1; continue; }
            };

            // No allocation and no managed code runs between the lookup above and this call,
            // which is what carries "alive when it was read" across to "alive at the call".
            //
            // The caller's answer is what separates a call from a key it only reached, and the reason
            // inside it picks the bucket: a decline cannot be a write, and a decline filed under a
            // cause it did not have is a future run sent after the wrong one (C11).
            tally.record(write(target, &value));
        }
    }

    let pass = APPLY_PASSES.fetch_add(1, Ordering::Relaxed) + 1;
    APPLY_TRANSLATIONS.fetch_add(tally.translations, Ordering::Relaxed);
    APPLY_KEYS.fetch_add(tally.keys, Ordering::Relaxed);
    APPLY_WRITES.fetch_add(tally.written, Ordering::Relaxed);
    APPLY_GONE.fetch_add(tally.gone, Ordering::Relaxed);
    APPLY_REKEYED.fetch_add(tally.rekeyed, Ordering::Relaxed);
    APPLY_NO_CALLABLE_ORIGINAL.fetch_add(tally.no_callable_original, Ordering::Relaxed);
    APPLY_NO_VALUE_IN_HAND.fetch_add(tally.no_value_in_hand, Ordering::Relaxed);
    report_pass(site, pass);

    tally
}

static INSTANCE: Lazy<Arc<SugoiClient>> = Lazy::new(|| {
    Arc::new(SugoiClient {
        agent: ureq::Agent::new_with_defaults(),
        url: Hachimi::instance().config.load().sugoi_url.as_ref()
            .map(|s| s.clone())
            .unwrap_or_else(|| "http://127.0.0.1:14366".to_owned()),
        request_lock: Mutex::new(()),
    })
});

pub static TRANSLATION_QUEUE: Lazy<(Sender<(String, String)>, Mutex<Receiver<(String, String)>>)> = Lazy::new(|| {
    let (tx, rx) = mpsc::channel();
    (tx, Mutex::new(rx))
});

pub static TRANSLATION_CACHE: Lazy<Mutex<FnvHashMap<String, String>>> = Lazy::new(|| {
    Mutex::new(FnvHashMap::default())
});

pub static PENDING_TRANSLATIONS: Lazy<Mutex<FnvHashSet<String>>> = Lazy::new(|| {
    Mutex::new(FnvHashSet::default())
});

pub static REQUEST_QUEUE: Lazy<Sender<String>> = Lazy::new(|| {
    let (tx, rx) = mpsc::channel::<String>();
    let translation_tx = TRANSLATION_QUEUE.0.clone();

    std::thread::Builder::new()
        .name("sugoi_worker".into())
        .spawn(move || {
            while let Ok(original) = rx.recv() {
                let mut batch = vec![original];

                while batch.len() < 50 {
                    if let Ok(next) = rx.try_recv() {
                        batch.push(next);
                    } else {
                        break;
                    }
                }

                let client = SugoiClient::instance();

                match client.translate(&batch) {
                    Ok(translated) => {
                        let mut pending = PENDING_TRANSLATIONS.lock().unwrap_or_else(|e| e.into_inner());
                        for (orig, trans) in batch.into_iter().zip(translated.into_iter()) {
                            let _ = translation_tx.send((orig.clone(), trans));
                            pending.remove(&orig);
                        }
                    }
                    Err(_) => {
                        let mut pending = PENDING_TRANSLATIONS.lock().unwrap_or_else(|e| e.into_inner());
                        for orig in batch {
                            pending.remove(&orig);
                        }
                    }
                }
            }
        })
        .expect("Failed to spawn sugoi_worker thread");

    tx
});

impl SugoiClient {
    pub fn instance() -> Arc<Self> {
        INSTANCE.clone()
    }

    pub fn get_cached(&self, original: &str) -> Option<String> {
        TRANSLATION_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(original).cloned()
    }

    pub fn translate_async(&self, original: String) {
        if self.get_cached(&original).is_some() {
            return;
        }

        let mut pending = PENDING_TRANSLATIONS.lock().unwrap_or_else(|e| e.into_inner());
        if pending.insert(original.clone()) {
            let _ = REQUEST_QUEUE.send(original);
        }
    }

    pub fn translate(&self, content: &[String]) -> Result<Vec<String>, Error> {
        let _guard = self.request_lock.lock().unwrap();

        let res = self.agent.post(&self.url)
            .header("Content-Type", "application/json")
            .header("Connection", "close")
            .send_json(Message::TranslateSentences { content })?;

        let body_str = res.into_body().read_to_string()?; 
        Ok(serde_json::from_str(&body_str)?)
    }

    pub fn translate_one(&self, content: String) -> Result<String, Error> {
        let mut res = self.translate(&[content])?;
        if res.len() != 1 {
            return Err(Error::RuntimeError("Server returned invalid amount of translated content".to_owned()));
        }
        Ok(res.pop().unwrap())
    }
}

#[derive(Serialize)]
#[serde(tag = "message")]
enum Message<'a> {
    #[serde(rename = "translate sentences")]
    TranslateSentences {
        content: &'a [String]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    // Stand-in for a registry entry. A unit test cannot build an `Il2CppObject` or a `GCHandle` -
    // no il2cpp call is reachable here (AGENTS section 4) - so the address an entry answers with,
    // and whether it still answers at all, are values the test owns.
    struct StandIn {
        original: String,
        address: Cell<usize>
    }

    impl TrackedComponent for StandIn {
        fn original(&self) -> &str {
            &self.original
        }

        fn live_target(&self) -> *mut Il2CppObject {
            self.address.get() as *mut Il2CppObject
        }
    }

    impl StandIn {
        fn new(original: &str, address: usize) -> Self {
            Self { original: original.to_owned(), address: Cell::new(address) }
        }

        /// The game let this component go: the handle now answers null.
        fn retire(&mut self) {
            self.address.set(0);
        }
    }

    fn three_components_of_one_string() -> Mutex<FnvHashMap<usize, StandIn>> {
        Mutex::new(FnvHashMap::from_iter([
            (0x1000usize, StandIn::new("hello", 0xA000)),
            (0x2000, StandIn::new("hello", 0xB000)),
            (0x3000, StandIn::new("hello", 0xC000)),
        ]))
    }

    /// The key one of the three addresses was recorded under, so a test can act on the entry a
    /// write has *not* reached yet without depending on the order the map iterates in.
    fn key_of(component: usize) -> usize {
        match component { 0xA000 => 0x1000, 0xB000 => 0x2000, _ => 0x3000 }
    }

    fn done(original: &str, translated: &str) -> Vec<(String, String)> {
        vec![(original.to_owned(), translated.to_owned())]
    }

    // The apply counters are process wide totals, and `cargo test` runs cases on several threads, so
    // every case that drives a pass takes a turn of its own. The case that reads the numbers the
    // apply line prints cannot tell its own pass from another thread's pass any other way.
    static APPLY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn apply_turn() -> MutexGuard<'static, ()> {
        APPLY_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every key a pass looked up ends up in exactly one of the five counts. A sixth outcome - a key
    /// that was neither called through nor refused - is the check-then-use shape coming back, and a
    /// write counted for a caller that declined is a count claiming a call that never happened.
    fn assert_every_key_is_accounted_for(tally: &PassTally) {
        let accounted = tally.accounted();
        assert_eq!(tally.keys, accounted,
            "of {} keys looked up, {} were written, {} were gone, {} held other text, {} had no \
             callable original behind the hook and {} had no value in hand: {} keys were neither \
             called through nor refused, which is a use with no check behind it",
            tally.keys, tally.written, tally.gone, tally.rekeyed, tally.no_callable_original,
            tally.no_value_in_hand, tally.keys.saturating_sub(accounted));
    }

    #[test]
    fn a_component_the_game_retired_during_an_earlier_write_is_not_written() {
        let _turn = apply_turn();
        let tracker = three_components_of_one_string();
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                used.borrow_mut().push(component as usize);

                // What one write costs the game: the original `set_text` runs, and original methods
                // close views and recycle pooled elements. Everything the pass has not reached yet
                // is gone by the time it gets there.
                let mut registry = shared_registry(&tracker);
                for (_, entry) in registry.iter_mut() {
                    entry.retire();
                }

                WriteAnswer::Written
            }
        );

        let used = used.take();
        assert_eq!(tally.written, 1, "the pass wrote {} components its own first write had already retired", tally.written);
        assert_eq!(tally.gone, 2, "the pass retired two components and its own lookup did not refuse them: {used:x?}");
        assert_every_key_is_accounted_for(&tally);
        assert_eq!(used.len(), 1, "the pass called through retired addresses: {used:x?}");
        assert!(matches!(used[0], 0xA000 | 0xB000 | 0xC000), "the one write went to {:#x}", used[0]);
    }

    #[test]
    fn the_snapshot_shape_this_fix_retired_called_through_addresses_it_had_retired_itself() {
        // The reproduction this item closes on, kept where it can be read against what replaces it.
        // This is `Text::apply_translations` as it stood at commit 299c64a, on the same shape of
        // registry as `a_component_the_game_retired_during_an_earlier_write_is_not_written` and with
        // the same game behaviour: `retain` asks every entry's handle whether the component is
        // alive, the list collects the map key of every match, the lock goes away, and the walk
        // calls with whatever the list holds.
        //
        // The keys are the addresses `set_text_hook` recorded, so they are the components. That is
        // the point: the value checked was the handle, the value used was the key.
        let tracker = Mutex::new(FnvHashMap::from_iter([
            (0xA000usize, StandIn::new("hello", 0xA000)),
            (0xB000, StandIn::new("hello", 0xB000)),
            (0xC000, StandIn::new("hello", 0xC000)),
        ]));
        let completed = done("hello", "Bonjour");

        let mut updates: Vec<(usize, String)> = Vec::new();
        {
            let mut registry = shared_registry(&tracker);
            registry.retain(|_, entry| !entry.live_target().is_null());

            for (original, translated) in &completed {
                for (&address, entry) in registry.iter() {
                    if entry.original() == original.as_str() {
                        updates.push((address, translated.clone()));
                    }
                }
            }
        }

        let mut calls: Vec<(usize, bool)> = Vec::new();

        for (address, _translated) in &updates {
            let alive_at_the_call = {
                let registry = shared_registry(&tracker);
                registry.get(address).is_some_and(|entry| !entry.live_target().is_null())
            };

            calls.push((*address, alive_at_the_call));

            let mut registry = shared_registry(&tracker);
            for (_, entry) in registry.iter_mut() {
                entry.retire();
            }
        }

        assert_eq!(updates.len(), 3, "every component matched, so the list held all three");

        let retired_calls = calls.iter().filter(|(_, alive)| !alive).count();
        assert_eq!(retired_calls, 2,
            "the snapshot walked {calls:x?}; two of those calls were made through components the \
             pass's own first write had already retired");
    }

    #[test]
    fn a_registry_key_is_never_the_value_the_pass_calls() {
        let _turn = apply_turn();
        // The map key is the address the component had when `set_text` recorded it. The pass uses it
        // to find the entry and nothing else; the value it calls with is what the entry's handle
        // answers now, which is a different number.
        let tracker = Mutex::new(FnvHashMap::from_iter([(0x1000usize, StandIn::new("hello", 0xA000))]));
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                used.borrow_mut().push(component as usize);
                WriteAnswer::Written
            }
        );

        assert_eq!(tally.written, 1);
        assert_eq!(tally.no_callable_original, 0, "a caller that called through its one component was tallied as having no hook");
        assert_eq!(tally.no_value_in_hand, 0, "a caller that called through its one component was tallied as having no value");
        assert_eq!(used.take(), vec![0xA000], "the pass wrote through the map key instead of the entry's own target");
    }

    #[test]
    fn a_key_the_game_handed_to_other_text_is_not_written() {
        let _turn = apply_turn();
        let tracker = three_components_of_one_string();
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                used.borrow_mut().push(component as usize);

                // The game re-texts every tracked component during that write. The keys this pass
                // collected are still keys of live entries - they are no longer *this* translation's
                // entries, and a key is not a promise about what lives under it.
                let mut registry = shared_registry(&tracker);
                for (_, entry) in registry.iter_mut() {
                    entry.original = String::from("something the game just wrote");
                }

                WriteAnswer::Written
            }
        );

        assert_eq!(tally.written, 1, "the pass wrote entries that no longer hold the string they were for");
        assert_eq!(tally.rekeyed, 2, "the pass found {} keys holding other text and wrote through {} of them", tally.rekeyed, tally.written);
        assert_every_key_is_accounted_for(&tally);
        assert_eq!(used.borrow().len(), 1);
    }

    #[test]
    fn a_key_the_scan_left_no_entry_under_is_neither_written_nor_called_through() {
        let _turn = apply_turn();
        let tracker = three_components_of_one_string();
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                used.borrow_mut().push(component as usize);

                // The other two components were destroyed by this write and their entries are out
                // of the registry already: the keys survive the scan, and at the lookup they answer
                // nothing. They are refused, and their addresses are never called.
                let mut registry = shared_registry(&tracker);
                let written = key_of(component as usize);
                registry.retain(|key, _| *key == written);

                WriteAnswer::Written
            }
        );

        assert_eq!(tally.written, 1);
        assert_eq!(tally.gone, 2, "two keys named no entry at the lookup and were not counted as refused");
        assert_eq!(tally.no_callable_original, 0, "a key the lookup refused was counted as a caller having no hook");
        assert_eq!(tally.no_value_in_hand, 0, "a key the lookup refused was counted as a caller having no value");
        assert_every_key_is_accounted_for(&tally);

        let used = used.take();
        assert_eq!(used.len(), 1, "the pass called through addresses whose entry was gone: {used:x?}");
    }

    #[test]
    fn one_finished_translation_is_prepared_once_however_many_components_take_it() {
        let _turn = apply_turn();
        let tracker = three_components_of_one_string();
        let prepared = Cell::new(0usize);
        let used: RefCell<Vec<(*mut Il2CppObject, String)>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| {
                prepared.set(prepared.get() + 1);
                translated.to_owned()
            },
            |component, text| -> WriteAnswer {
                used.borrow_mut().push((component, text.clone()));
                WriteAnswer::Written
            }
        );

        assert_eq!(prepared.get(), 1, "the pass built the value once per component, not per translation");
        assert_eq!(tally.written, 3);
        assert_eq!(tally.gone, 0, "the pass refused a component that was still there");
        assert_eq!(tally.rekeyed, 0, "the pass refused a component that was still this translation's");
        assert_every_key_is_accounted_for(&tally);

        let used = used.take();
        assert_eq!(used.len(), 3);
        for (component, text) in used {
            assert_eq!(text, "Bonjour");
            assert!(!component.is_null());
        }
    }

    #[test]
    fn a_poisoned_component_registry_answers_instead_of_panicking_the_pass() {
        let _turn = apply_turn();
        let tracker = three_components_of_one_string();
        let touched: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tripped = catch_unwind(AssertUnwindSafe(|| {
            let _guard = tracker.lock().unwrap();
            panic!("a detour body that blew up holding the component registry");
        }));
        assert!(tripped.is_err(), "the registry lock is poisoned on purpose");

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                touched.borrow_mut().push(component as usize);
                WriteAnswer::Written
            }
        );

        assert_eq!(tally.written, 3, "a poisoned registry still answers for the live components");
        assert_eq!(tally.no_callable_original, 0, "a caller that wrote was tallied as having no hook");
        assert_eq!(tally.no_value_in_hand, 0, "a caller that wrote was tallied as having no value");

        // A write count is only evidence when the caller behind it did the call. This stand-in
        // always can, so every counted write has a recorded component behind it and the two numbers
        // are the same number.
        assert_eq!(touched.borrow().len(), tally.written,
            "the pass counted {} writes and its caller recorded {} calls", tally.written, touched.borrow().len());
    }

    #[test]
    fn an_entry_that_moved_under_the_walk_is_written_where_it_lives_at_the_lookup() {
        let _turn = apply_turn();
        // The other writer of the check-then-use shape: something runs during the walk and moves a
        // component out from under a key the pass has already collected. The key is still a key;
        // the address the entry answers with is no longer the address the scan saw. A pass holding
        // the scan's value calls the old address, and the retired `Vec<(address, string)>` shape
        // did exactly that - it called with the key.
        let tracker = Mutex::new(FnvHashMap::from_iter([
            (0x1000usize, StandIn::new("hello", 0xA000)),
            (0x2000, StandIn::new("hello", 0xB000)),
        ]));
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                used.borrow_mut().push(component as usize);

                // Both entries move as the cost of this write, each to its own new address. Only
                // the entry this write is for may still be seen where it was.
                let mut registry = shared_registry(&tracker);
                if let Some(entry) = registry.get_mut(&0x1000) { entry.address.set(0xD000); }
                if let Some(entry) = registry.get_mut(&0x2000) { entry.address.set(0xE000); }

                WriteAnswer::Written
            }
        );

        let used = used.take();
        assert_eq!(tally.written, 2, "two live entries, {} written", tally.written);
        assert!(used.iter().any(|address| *address == 0xD000 || *address == 0xE000),
            "no moved entry was written where its handle answered at the lookup: {used:x?}");
        assert!(!(used.contains(&0xA000) && used.contains(&0xB000)),
            "the pass wrote both components at the addresses the scan collected, although one of \
             them moved before the pass reached it: {used:x?}");
        assert_every_key_is_accounted_for(&tally);
    }

    /// The C11 report's own evidence, run against the shape that broke it. Both shipped callers
    /// answer their guarded trampoline lookup with a bare `return` when the hook was never armed,
    /// when `_addr is null`, or when the hook is named in `disabled_hooks` (C1). The component was
    /// live and the entry was still this translation's, and the game received nothing. A write count
    /// that includes those keys is a count of work the mod did not do.
    #[test]
    fn a_caller_that_refused_every_write_reports_no_write_to_the_apply_line() {
        let _turn = apply_turn();

        let written_before = APPLY_WRITES.load(Ordering::Relaxed);
        let no_hook_before = APPLY_NO_CALLABLE_ORIGINAL.load(Ordering::Relaxed);
        let no_value_before = APPLY_NO_VALUE_IN_HAND.load(Ordering::Relaxed);

        let tracker = three_components_of_one_string();
        let called: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |_component, _text| -> WriteAnswer {
                // `let Some(set_text) = get_orig_fn_guarded!(set_text_hook, SetTextFn) else { return
                // WriteAnswer::NoCallableOriginal }`: this wrapper has no trampoline and no callable
                // target behind it, so the caller says no and calls nothing.
                WriteAnswer::NoCallableOriginal
            }
        );

        assert_eq!(tally.keys, 3, "the guard reached three live entries");
        assert_eq!(tally.written, 0, "the pass called through nothing and reported {} written", tally.written);
        assert_eq!(tally.no_callable_original, 3, "three refusals were tallied as {}", tally.refused());
        assert_eq!(tally.no_value_in_hand, 0,
            "a caller that never reached a string had {} refusals filed against the string",
            tally.no_value_in_hand);
        assert_eq!(tally.gone, 0, "a caller with no hook made {} components gone", tally.gone);
        assert_eq!(tally.rekeyed, 0, "a caller with no hook made {} keys hold other text", tally.rekeyed);
        assert_eq!(called.borrow().len(), 0, "the tally reports no write and the caller made {} calls", called.borrow().len());
        assert_every_key_is_accounted_for(&tally);

        // The apply line reads these totals, so the line a run reads for "translations landed" moves
        // by zero for a session that received nothing, and the refusals are visible in it as the
        // reason they happened.
        assert_eq!(APPLY_WRITES.load(Ordering::Relaxed), written_before,
            "the total the apply line prints as written moved for a pass that called through nothing");
        assert_eq!(APPLY_NO_CALLABLE_ORIGINAL.load(Ordering::Relaxed), no_hook_before + 3,
            "the refusals are nowhere a run can read them");
        assert_eq!(APPLY_NO_VALUE_IN_HAND.load(Ordering::Relaxed), no_value_before,
            "the line would name a translated string for a pass whose caller stopped at its hook");
    }

    /// A refusal is one key's fact and not the pass's: the writes around it keep their count, and
    /// every key still lands in exactly one bucket.
    #[test]
    fn a_write_the_caller_refused_between_two_it_made_is_counted_as_refused() {
        let _turn = apply_turn();

        let tracker = three_components_of_one_string();
        let reached = Cell::new(0usize);
        let called: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| -> WriteAnswer {
                reached.set(reached.get() + 1);

                // One of the three has no callable original behind this wrapper; the other two do.
                if component as usize == 0xB000 {
                    return WriteAnswer::NoCallableOriginal;
                }

                called.borrow_mut().push(component as usize);
                WriteAnswer::Written
            }
        );

        assert_eq!(reached.get(), 3, "the pass reached {} live entries", reached.get());
        assert_eq!(tally.written, 2, "two calls were made and the pass reported {} written", tally.written);
        assert_eq!(tally.no_callable_original, 1, "the one refusal was counted with the writes");
        assert_eq!(tally.no_value_in_hand, 0, "a caller that never looked at a value filed {} against the value", tally.no_value_in_hand);
        assert_eq!(called.borrow().len(), tally.written,
            "the pass reported {} written and the caller made {} calls", tally.written, called.borrow().len());
        assert!(called.borrow().iter().all(|address| *address != 0xB000),
            "the entry with no hook behind it was tallied as written: {:x?}", called.borrow());
        assert_every_key_is_accounted_for(&tally);
    }

    /// The reproduction this item closes on, kept where it can be read against what replaces it.
    /// The retired pass ran `write(target, &value); tally.written += 1` with a `write` that returned
    /// nothing, so a caller could not say no and every key the lookup answered live for became a
    /// write. Same registry, same refusal, both shapes side by side.
    #[test]
    fn the_retired_counting_tallied_every_write_a_caller_refused_as_a_written_component() {
        let _turn = apply_turn();

        // Both halves run against the same state: three live entries for this translation, and no
        // callable original behind the wrapper (the state `get_orig_fn_guarded!` answers None in,
        // which is where Text.rs and TextMesh.rs stop today). `reached` is the component the caller
        // was handed and `calls` the call it actually made, so the gap between the pass's count and
        // the game's is measured here instead of asserted.
        let hook_callable = Cell::new(false);

        let retired_tracker = three_components_of_one_string();
        let retired_reached = Cell::new(0usize);
        let retired_calls = Cell::new(0usize);
        let mut retired_written = 0usize;

        // The retired `write`: `FnMut(*mut Il2CppObject, &V)`, answering nothing. A refusal and a
        // call are the same event to a pass that cannot be told.
        let retired_write = |_component: *mut Il2CppObject, _value: &String| {
            retired_reached.set(retired_reached.get() + 1);

            if hook_callable.get() {
                retired_calls.set(retired_calls.get() + 1);
            }
        };

        let keys: Vec<usize> = {
            let registry = shared_registry(&retired_tracker);
            registry.iter()
                .filter(|(_, entry)| entry.original() == "hello")
                .map(|(key, _)| *key)
                .collect()
        };

        let value = String::from("Bonjour");

        for key in &keys {
            match answer_key(&retired_tracker, *key, "hello") {
                KeyAnswer::Live(target) => {
                    retired_write(target, &value);
                    retired_written += 1;
                }
                KeyAnswer::Gone | KeyAnswer::Rekeyed => continue,
            }
        }

        assert_eq!(keys.len(), 3, "three live entries matched this translation");
        assert_eq!(retired_reached.get(), 3, "the caller was handed {} components", retired_reached.get());
        assert_eq!(retired_calls.get(), 0, "the caller called through {} of them", retired_calls.get());
        assert_eq!(retired_written, 3,
            "the retired pass reported {} writes to the apply line for a game that received {}",
            retired_written, retired_calls.get());

        // What replaces it: the same three keys and the same refusal, and the count the apply line
        // reads stays where the game is.
        let shipped_tracker = three_components_of_one_string();
        let shipped_reached = Cell::new(0usize);
        let shipped_calls = Cell::new(0usize);

        let tally = apply_translation_pass(
            "Test",
            &shipped_tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |_component, _text| -> WriteAnswer {
                shipped_reached.set(shipped_reached.get() + 1);

                if !hook_callable.get() {
                    return WriteAnswer::NoCallableOriginal;
                }

                shipped_calls.set(shipped_calls.get() + 1);
                WriteAnswer::Written
            }
        );

        assert_eq!(tally.keys, 3, "the guard still reached all three live entries");
        assert_eq!(shipped_reached.get(), 3, "the caller was handed {} components", shipped_reached.get());
        assert_eq!(shipped_calls.get(), 0, "the caller called through {} of them", shipped_calls.get());
        assert_eq!(tally.written, 0, "the replacement reported {} writes for {} calls", tally.written, shipped_calls.get());
        assert_eq!(tally.no_callable_original, 3, "and put the {} refusals in a count of their own", tally.refused());
        assert_every_key_is_accounted_for(&tally);
    }

    /// Which bucket a decline lands in is decided by the answer the caller returns at the write and
    /// routed by `PassTally::record`, never guessed by the pass. Driven straight on the tally because
    /// the routing is exactly what decides which count a run reads.
    #[test]
    fn each_write_answer_lands_in_the_count_for_its_own_reason() {
        let mut tally = PassTally::default();

        tally.record(WriteAnswer::Written);
        tally.record(WriteAnswer::NoCallableOriginal);
        tally.record(WriteAnswer::NoValueInHand);

        assert_eq!(tally.written, 1, "a completed call was not counted as a write");
        assert_eq!(tally.no_callable_original, 1, "a missing hook landed somewhere other than its own count");
        assert_eq!(tally.no_value_in_hand, 1, "a declined string landed somewhere other than its own count");
        assert_eq!(tally.refused(), 2, "the two refusals sum to {}", tally.refused());
        assert_eq!(tally.accounted(), 3, "the routing put three answers in {} buckets", tally.accounted());
    }

    /// Both shipped callers decline twice over: once when `get_orig_fn_guarded!` answers None, and once
    /// on their `text.target().is_null()` guard (`Text::apply_translations`,
    /// `TextMesh::apply_translations`). A pass that files the second under the first prints a missing
    /// hook for a session that had its hook, so the two must stay apart even when they happen in one
    /// pass, in the shipped order, beside a write that worked.
    #[test]
    fn a_declined_string_and_a_missing_hook_are_not_the_same_refusal() {
        let _turn = apply_turn();

        let tracker = Mutex::new(FnvHashMap::from_iter([
            (0x1000usize, StandIn::new("hello", 0xA000)),
            (0x2000, StandIn::new("hello", 0xB000)),
            (0x3000, StandIn::new("world", 0xC000)),
        ]));
        let called: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        // One translation the game string is there for, one where it is not: the value stands in for
        // the strong handle `prepare` builds, at the address that handle answers for.
        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &[("hello".to_owned(), "Bonjour".to_owned()), ("world".to_owned(), "Bonjour le monde".to_owned())],
            |translated| if translated == "Bonjour" { 0xF000usize as *mut Il2CppObject } else { std::ptr::null_mut() },
            |component, value| -> WriteAnswer {
                // The shipped order: this wrapper's trampoline first, then the value it was handed.
                // 0xB000 has no callable original behind the wrapper, and the second translation has
                // the hook and no string in hand.
                if component as usize == 0xB000 {
                    return WriteAnswer::NoCallableOriginal;
                }

                if value.is_null() {
                    return WriteAnswer::NoValueInHand;
                }

                called.borrow_mut().push(component as usize);
                WriteAnswer::Written
            }
        );

        assert_eq!(tally.keys, 3, "the pass looked up {} keys", tally.keys);
        assert_eq!(tally.written, 1, "the pass reported {} writes for {} calls", tally.written, called.borrow().len());
        assert_eq!(tally.no_callable_original, 1, "a caller with no hook behind its wrapper was not tallied as one");
        assert_eq!(tally.no_value_in_hand, 1, "a caller with the hook and no string was not tallied as one");
        assert_eq!(tally.refused(), 2, "two live entries were refused: {} for the hook and {} for the string",
            tally.no_callable_original, tally.no_value_in_hand);
        assert_eq!(called.take(), vec![0xA000], "the calls the tally says happened were not the calls made");
        assert_every_key_is_accounted_for(&tally);
    }

    /// The closure rule a run follows reads the two totals the apply line prints. A pass whose caller
    /// stopped at a null string must leave the hook total alone, or the line names a `set_text_hook`
    /// the backend never refused and the next run goes looking for a take-down that did not happen.
    #[test]
    fn the_apply_line_blames_a_missing_hook_only_when_the_hook_total_moves() {
        let _turn = apply_turn();

        let written_before = APPLY_WRITES.load(Ordering::Relaxed);
        let no_hook_before = APPLY_NO_CALLABLE_ORIGINAL.load(Ordering::Relaxed);
        let no_value_before = APPLY_NO_VALUE_IN_HAND.load(Ordering::Relaxed);

        let tracker = three_components_of_one_string();

        let tally = apply_translation_pass(
            "Test",
            &tracker,
            &done("hello", "Bonjour"),
            |_translated| std::ptr::null_mut::<Il2CppObject>(),
            |_component, value| -> WriteAnswer {
                // The shipped guards in the shipped order. This wrapper's guard answered Some for
                // every component here, so the caller's only decline is `text.target().is_null()`.
                if value.is_null() {
                    return WriteAnswer::NoValueInHand;
                }

                WriteAnswer::Written
            }
        );

        assert_eq!(tally.written, 0, "a null string was tallied as {} writes", tally.written);
        assert_eq!(tally.no_callable_original, 0,
            "a pass whose caller held its hook all three times reported {} live entries with no hook",
            tally.no_callable_original);
        assert_eq!(tally.no_value_in_hand, 3, "three live entries with no string were not tallied as such");
        assert_every_key_is_accounted_for(&tally);

        assert_eq!(APPLY_WRITES.load(Ordering::Relaxed), written_before,
            "the written total moved for a pass that called through nothing");
        assert_eq!(APPLY_NO_CALLABLE_ORIGINAL.load(Ordering::Relaxed), no_hook_before,
            "the apply line would name a missing hook for a session whose hook answered");
        assert_eq!(APPLY_NO_VALUE_IN_HAND.load(Ordering::Relaxed), no_value_before + 3,
            "the reason this pass actually ran into is nowhere a run can read it");
    }

    /// The reproduction this item closes on, kept where it can be read against what replaces it. The
    /// retired fold was `if write(..) { written += 1 } else { caller_refused += 1 }`, and the single
    /// bucket it filled was documented as a caller that "had no callable original behind its own
    /// hook". Both shipped callers also return false for a null translated string, so a session with a
    /// working hook and a string the game did not take printed exactly the number a session with a
    /// taken-down hook printed, and the closure rule attached to that field sent the next run after a
    /// refused `set_text_hook` restore. Same answers, both shapes side by side.
    #[test]
    fn the_retired_single_bucket_answered_a_declined_string_exactly_like_a_missing_hook() {
        let hook_taken_down = WriteAnswer::NoCallableOriginal;
        let string_declined = WriteAnswer::NoValueInHand;

        let mut printed: Vec<(WriteAnswer, usize, usize, usize, usize)> = Vec::new();

        for answer in [hook_taken_down, string_declined] {
            // The retired fold: one bucket for every decline, whatever the caller meant.
            let (mut retired_written, mut retired_refused) = (0usize, 0usize);
            for _ in 0..3 {
                if answer == WriteAnswer::Written {
                    retired_written += 1;
                } else {
                    retired_refused += 1;
                }
            }

            // What replaces it: the caller's reason picks the count.
            let mut now = PassTally::default();
            for _ in 0..3 {
                now.record(answer);
            }

            assert_eq!(now.accounted(), 3, "the routing lost a key: {}", now.accounted());
            printed.push((answer, retired_written, retired_refused, now.no_callable_original, now.no_value_in_hand));
        }

        let (_, _, hook_case_refused, hook_case_no_callable, hook_case_no_value) = printed[0];
        let (_, _, string_case_refused, string_case_no_callable, string_case_no_value) = printed[1];

        assert_eq!(hook_case_refused, string_case_refused,
            "the retired line printed {} refused for a taken-down hook and {} for a declined string, \
             and its documentation named the hook for both",
            hook_case_refused, string_case_refused);
        assert_eq!(string_case_no_callable, 0,
            "the replacement filed a declined string under {} live entries with no callable original",
            string_case_no_callable);
        assert_eq!(string_case_no_value, 3, "the declined string is not in the count a run can read: {}", string_case_no_value);
        assert_eq!(hook_case_no_value, 0,
            "the replacement filed a missing hook under {} live entries with no translated string",
            hook_case_no_value);
        assert_eq!(hook_case_no_callable, 3, "the taken-down hook is not in the count a run can read: {}", hook_case_no_callable);
    }

    #[test]
    fn the_apply_line_is_the_fork_s_first_n_then_every_n_pattern() {
        // A line per pass would be a line per finished translation, which AGENTS section 6 rules
        // out; a line only at the end of a session is a line a run cannot compare against the
        // screens it played. This is the cadence `AnimationSpeed`'s apply line uses.
        let logged: Vec<usize> = (1..=PASS_CHUNK * 2).filter(|pass| pass_is_worth_logging(*pass)).collect();
        assert_eq!(logged, vec![1, 2, 3, 4, 5, 6, 64, 128],
            "the apply line is not the fork's first {PASS_DETAIL_LIMIT} then every {PASS_CHUNK} pattern");
    }
}
