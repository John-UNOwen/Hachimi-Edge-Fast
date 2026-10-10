use std::sync::{Arc, Mutex, MutexGuard, mpsc::{self, Receiver, Sender}};

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

/// Push finished translations into every tracked component that still holds the string they were
/// translated from (C11).
///
/// The pass takes **keys** out of the registry, never targets: a key is the identity an entry is
/// stored under, not a value to call with. For each key it goes back into the registry, takes the
/// entry still sitting under that key, asks that entry for its live target, and calls with what it
/// answered - so the pointer used is the pointer that was just read, and it was read at the moment
/// of the call, with nothing in between that could have run the component away.
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
/// the pass a unit test drives: AGENTS section 4, a test proves the decision and never the cost.
pub fn apply_translation_pass<E, V, P, W>(
    tracker: &Mutex<FnvHashMap<usize, E>>,
    completed: &[(String, String)],
    prepare: P,
    mut write: W,
) -> usize
where
    E: TrackedComponent,
    P: Fn(&str) -> V,
    W: FnMut(*mut Il2CppObject, &V),
{
    let mut written = 0;
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

        for key in &keys {
            let target = {
                let registry = shared_registry(tracker);

                registry.get(key)
                    // A key is an identity only. If the game handed that address to a different
                    // component since the scan - freed addresses get reused (C8) - this is not the
                    // entry this translation was for, so it is not written.
                    .filter(|entry| entry.original() == original.as_str())
                    .map(|entry| entry.live_target())
            };

            let Some(target) = target.filter(|target| !target.is_null()) else { continue };

            // No allocation and no managed code runs between the derivation above and this call,
            // which is what carries "alive when it was read" across to "alive at the call".
            write(target, &value);
            written += 1;
        }
    }

    written
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

    fn done(original: &str, translated: &str) -> Vec<(String, String)> {
        vec![(original.to_owned(), translated.to_owned())]
    }

    #[test]
    fn a_component_the_game_retired_during_an_earlier_write_is_not_written() {
        let tracker = three_components_of_one_string();
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let written = apply_translation_pass(
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| {
                used.borrow_mut().push(component as usize);

                // What one write costs the game: the original `set_text` runs, and original methods
                // close views and recycle pooled elements. Everything the pass has not reached yet
                // is gone by the time it gets there.
                let mut registry = shared_registry(&tracker);
                for (_, entry) in registry.iter_mut() {
                    entry.retire();
                }
            }
        );

        let used = used.take();
        assert_eq!(written, 1, "the pass wrote {} components its own first write had already retired", written);
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
        // The map key is the address the component had when `set_text` recorded it. The pass uses it
        // to find the entry and nothing else; the value it calls with is what the entry's handle
        // answers now, which is a different number.
        let tracker = Mutex::new(FnvHashMap::from_iter([(0x1000usize, StandIn::new("hello", 0xA000))]));
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let written = apply_translation_pass(
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| used.borrow_mut().push(component as usize)
        );

        assert_eq!(written, 1);
        assert_eq!(used.take(), vec![0xA000], "the pass wrote through the map key instead of the entry's own target");
    }

    #[test]
    fn a_key_the_game_handed_to_other_text_is_not_written() {
        let tracker = three_components_of_one_string();
        let used: RefCell<Vec<usize>> = RefCell::new(Vec::new());

        let written = apply_translation_pass(
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |component, _text| {
                used.borrow_mut().push(component as usize);

                // The game re-texts every tracked component during that write. The keys this pass
                // collected are still keys of live entries - they are no longer *this* translation's
                // entries, and a key is not a promise about what lives under it.
                let mut registry = shared_registry(&tracker);
                for (_, entry) in registry.iter_mut() {
                    entry.original = String::from("something the game just wrote");
                }
            }
        );

        assert_eq!(written, 1, "the pass wrote entries that no longer hold the string they were for");
        assert_eq!(used.borrow().len(), 1);
    }

    #[test]
    fn one_finished_translation_is_prepared_once_however_many_components_take_it() {
        let tracker = three_components_of_one_string();
        let prepared = Cell::new(0usize);
        let used: RefCell<Vec<(*mut Il2CppObject, String)>> = RefCell::new(Vec::new());

        let written = apply_translation_pass(
            &tracker,
            &done("hello", "Bonjour"),
            |translated| {
                prepared.set(prepared.get() + 1);
                translated.to_owned()
            },
            |component, text| used.borrow_mut().push((component, text.clone()))
        );

        assert_eq!(prepared.get(), 1, "the pass built the value once per component, not per translation");
        assert_eq!(written, 3);

        let used = used.take();
        assert_eq!(used.len(), 3);
        for (component, text) in used {
            assert_eq!(text, "Bonjour");
            assert!(!component.is_null());
        }
    }

    #[test]
    fn a_poisoned_component_registry_answers_instead_of_panicking_the_pass() {
        let tracker = three_components_of_one_string();

        let tripped = catch_unwind(AssertUnwindSafe(|| {
            let _guard = tracker.lock().unwrap();
            panic!("a detour body that blew up holding the component registry");
        }));
        assert!(tripped.is_err(), "the registry lock is poisoned on purpose");

        let written = apply_translation_pass(
            &tracker,
            &done("hello", "Bonjour"),
            |translated| translated.to_owned(),
            |_component, _text| ()
        );

        assert_eq!(written, 3, "a poisoned registry still answers for the live components");
    }
}