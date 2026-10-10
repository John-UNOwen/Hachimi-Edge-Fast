use std::{
    sync::{Mutex, atomic::{AtomicUsize, Ordering}},
};

use fnv::FnvHashMap;
use once_cell::sync::Lazy;
use widestring::Utf16Str;

use crate::{core::{ext::Utf16StringExt, hachimi::{AssetMetadata, recover_lock}}, il2cpp::{
    api::{il2cpp_array_new, il2cpp_resolve_icall}, ext::{Il2CppObjectExt, Il2CppStringExt}, hook::{
        mscorlib::Byte,
        umamusume::{StoryParamChangeEffect, StoryRaceTextAsset, StoryTimelineData, TextDotData, TextRubyData},
        Cute_UI_Assembly::AtlasReference,
        UnityEngine_CoreModule::{GameObject, Texture2D, Object}
    }, sql::{self, MetaData, MetaIdentity, TableRead}, symbols::GCHandle, types::*
}};

pub const ASSET_PATH_PREFIX: &str = "assets/_gallopresources/bundle/resources/";

pub struct RequestInfo {
    pub name_handle: GCHandle,
    // Held weak rather than as a bare address: `GetResult` can run after the game has freed the
    // bundle the request was started on, and IL2CPP reuses freed addresses (C8). `on_LoadAsset`
    // reads the bundle's own name to confirm which bundle an asset patch was authored for (C10), so
    // a stale address here would be a read of a freed object rather than a name.
    bundle_handle: GCHandle,
}
impl RequestInfo {
    pub fn name(&self) -> *mut Il2CppString {
        self.name_handle.target() as _
    }

    /// The bundle the request was started on, or null once the game has freed it.
    pub fn bundle(&self) -> *mut Il2CppObject {
        self.bundle_handle.target() as _
    }
}
pub static REQUEST_INFOS: Lazy<Mutex<FnvHashMap<usize, RequestInfo>>> = Lazy::new(|| Mutex::default());

/// What a patch's recorded bundle identity says about the bundle a load actually came from.
///
/// `expected` is the `bundle_name` the patch's own metadata records (`AssetMetadata`): the bundle
/// the patch was authored against. C10 is the hole these variants close. Upstream `e01be04` took
/// the load path recording (`BUNDLE_PATHS`, written by `LoadFromFile_Internal`) out of the guard
/// and left the game's own `meta` table as the only way to resolve an identity; upstream `7ae4577`
/// then answered `true` for every region that table does not cover - "Unsolved for other regions
/// for now" - which is every run on this fork's primary target (Windows Steam Global,
/// `src/windows/game_impl.rs:25`). A patch then went to whatever asset the load handed back, and
/// upstream issue #56 names the symptom: "Hashes are ignored, causing modifications to always
/// apply. This is particularly noticeable with atlas updates, where it leads to jumbled textures."
///
/// What the four variants are built from is measured on the primary target, not assumed:
///
/// - A shipped package records a bundle as an id: UmaTL English's `assets/atlas/common/common.json`
///   records `{"windows":{"bundle_name":"ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"}}`, and a Global install
///   names its bundles by exactly that shape - `<Persistent>/dat/<2 chars>/<32 char id>`, 171,845
///   files, none with an extension. So a recorded identity is comparable name to name, with no table
///   in the way, and that is the confirmation that actually fires here.
/// - A differing id on its own is not a mismatch. It is what a package authored against another
///   install looks like - a UI atlas bakes its text in, so the same bundle hashes differently between
///   the Japan and Global packages, and the id above is not a file in this install at all - and it is
///   indistinguishable from a bundle whose contents changed. `Mismatch` therefore requires this
///   client's own table to name both sides as two different logical bundles. Refusing on less than
///   that is how an identity check turns into atlas replacements silently not applying for the users
///   the patches were written for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchIdentity {
    /// The bundle standing in front of the guard is the one the patch was authored for.
    Confirmed,
    /// The data package records no identity for this platform, so the patch states nothing to
    /// confirm. Per upstream #56 that is how a package opts out of guarding: the recorded value is
    /// per platform, and an absent one is not a refusal.
    Unrecorded,
    /// This client's table names both bundles and they are different bundles. The only state that
    /// refuses a patch: the #56 jumbled-atlas case, caught on knowledge rather than on a guess.
    Mismatch {
        expected: String,
        expected_logical: String,
        observed: String,
        observed_logical: String,
    },
    /// A value is recorded that this client cannot resolve against the bundle in front of it. That is
    /// not a confirmation, but it is not evidence of a difference either, so it stays an apply - the
    /// outcome `7ae4577` reached by giving up on the region - and it is said out loud, bounded, by
    /// [`note_unconfirmed`] instead of passing unnoticed.
    Unconfirmed { expected: String, observed: String },
}

impl PatchIdentity {
    /// Whether the handlers may write the patch. Refusal is reserved for what this client can
    /// positively identify as the wrong bundle.
    pub fn may_apply(&self) -> bool {
        !matches!(self, PatchIdentity::Mismatch { .. })
    }
}

/// What a bundle that names nothing is called in an identity log line.
const NO_BUNDLE_NAME: &str = "<bundle reports no name>";

/// The pure half of [`check_asset_bundle_name`]: no game object, no table read, no log.
///
/// `observed` is the name this client reports for the bundle (`UnityEngine.Object.name`);
/// `table_of` is what this client's own `meta` table says about it (`sql::MetaData::identity_of`).
/// The name forms are the whole name, its last path component, and that component without an
/// extension: the shapes a bundle is really reported under, and the same set the table's keys are
/// built from (`sql::index_logical_name`). Upstream built its lookup key as `<last component of n>.a`
/// against a table whose `n` has no extension, so it could only be reached by a name carrying that
/// extension - and a bundle named with no extension, which is what this install's bundles are, could
/// never reach a row at all.
pub fn check_patch_identity(
    expected: Option<&str>,
    observed: Option<&str>,
    table_of: impl FnOnce(&str) -> MetaIdentity,
) -> PatchIdentity {
    let Some(expected) = expected else {
        return PatchIdentity::Unrecorded;
    };
    let Some(observed) = observed else {
        return PatchIdentity::Unconfirmed { expected: expected.to_owned(), observed: NO_BUNDLE_NAME.to_owned() };
    };

    // Name to name first: whoever built the patch records the bundle as the whole name this client
    // reports, as its last path component, or as that component without its extension. All three name
    // the same bundle, so any of them confirms it.
    if observed == expected
        || sql::bundle_file_name(observed) == expected
        || sql::bundle_file_stem(observed) == expected
    {
        return PatchIdentity::Confirmed;
    }

    match table_of(observed) {
        MetaIdentity::Same => PatchIdentity::Confirmed,
        MetaIdentity::Different { expected_logical, observed_logical } => PatchIdentity::Mismatch {
            expected: expected.to_owned(),
            expected_logical,
            observed: observed.to_owned(),
            observed_logical,
        },
        MetaIdentity::Unknown => PatchIdentity::Unconfirmed {
            expected: expected.to_owned(),
            observed: observed.to_owned(),
        },
    }
}

/// The name half of the guard on the game's own UTF-16 name, in place: no `String` is built out of
/// the bundle's name to decide with, because this is the comparison a per asset load path actually
/// runs (AGENTS section 6). It has to agree with the `&str` half above, and
/// `the_two_name_halves_agree_on_the_same_forms` pins them together.
pub fn bundle_names_match(expected: &str, observed: &Utf16Str) -> bool {
    let file = observed.path_filename();
    observed.str_eq(expected) || file.str_eq(expected) || file.path_basename().str_eq(expected)
}

/// How many times one run says anything about an identity decision. The counters keep counting past
/// the limit, so the lines that were printed still carry the running total (AGENTS section 6: a path
/// walked per asset load logs a bounded number of times, never once per load).
const IDENTITY_LOG_LIMIT: usize = 20;

static IDENTITY_CONFIRMED_LOGS: AtomicUsize = AtomicUsize::new(0);
static IDENTITY_UNCONFIRMED_LOGS: AtomicUsize = AtomicUsize::new(0);
static IDENTITY_MISMATCH_LOGS: AtomicUsize = AtomicUsize::new(0);

fn within_identity_log_limit(counter: &AtomicUsize) -> Option<usize> {
    let hits = counter.fetch_add(1, Ordering::Relaxed) + 1;
    (hits <= IDENTITY_LOG_LIMIT).then_some(hits)
}

fn note_confirmed(expected: impl std::fmt::Display, observed: impl std::fmt::Display) {
    if let Some(hits) = within_identity_log_limit(&IDENTITY_CONFIRMED_LOGS) {
        info!("Bundle identity confirmed: patch names {}, this client reports {} ({} confirmed so far)", expected, observed, hits);
    }
}

/// The loud half of C10: an identity this client cannot resolve is applied *and* reported, naming both
/// sides, so a run can read which patches went in unconfirmed instead of having to work out whether the
/// guard silently stopped applying them.
fn note_unconfirmed(expected: impl std::fmt::Display, observed: impl std::fmt::Display) {
    if let Some(hits) = within_identity_log_limit(&IDENTITY_UNCONFIRMED_LOGS) {
        warn!(
            "Bundle identity unconfirmed, applying the patch: patch names {}, this client reports {} for the bundle being loaded ({} unconfirmed so far)",
            expected, observed, hits
        );
    }
}

fn note_mismatch(mismatch: &PatchIdentity) {
    if let Some(hits) = within_identity_log_limit(&IDENTITY_MISMATCH_LOGS) {
        if let PatchIdentity::Mismatch { expected, expected_logical, observed, observed_logical } = mismatch {
            warn!(
                "Bundle identity mismatch, patch refused: patch names {} ({}), this asset came from {} ({}) ({} mismatches so far)",
                expected, expected_logical, observed, observed_logical, hits
            );
        }
    }
}

pub fn check_asset_bundle_name(this: *mut Il2CppObject, metadata: &AssetMetadata) -> bool {
    let Some(expected) = metadata.bundle_name.as_deref() else {
        // Nothing recorded for this platform: there is no identity this guard could confirm.
        return true;
    };

    let name_ptr = if this.is_null() { std::ptr::null_mut() } else { Object::get_name(this) };
    let Some(name) = (unsafe { name_ptr.as_ref() }) else {
        // C9: the name is the game's own answer for the bundle object, and a bundle that names nothing
        // is a legitimate null - a bundle this client cannot name, not a wrong one.
        note_unconfirmed(expected, NO_BUNDLE_NAME);
        return true;
    };

    let observed = name.as_utf16str();

    if bundle_names_match(expected, observed) {
        note_confirmed(expected, observed);
        return true;
    }

    // Only then the game's own `meta` table, and only while there is one left to ask: a client whose
    // bounded tries are spent answers Unknown here without converting the name, taking a lock, calling
    // into the game or writing another log line. The tries are paid for by `sql::TableAttempts::authorise`
    // as `identity_of` is granted them - the open against `META_TABLE_ATTEMPTS_CAP`, the key bail
    // against `META_TABLE_KEY_BAILS_CAP`, the settled state's key watch against
    // `META_TABLE_KEY_WATCH_CAP` - so this skip is where the per-load cost actually ends rather than a
    // bound some later lookup was free to ignore (`sql::tests::a_settled_guard_pays_for_the_lock_it_takes_and_a_key_arriving_late_un_settles_it`).
    //
    // Note what the skip does *not* do: it applies. The settled state means this client cannot resolve
    // the identity, not that it resolved a difference, and refusing here is how a texture or atlas
    // replacement silently stops applying for the user the patch was written for. It is also not
    // permanent - a window is re-armed when the state machine witnesses the game's sqlite key arriving
    // (`sql::TableAttempts::re_arm_if_stale`), so a key that lands after the tries were spent does not
    // leave the whole session answering from the settled state.
    if MetaData::table_plan() == TableRead::GiveUp {
        note_unconfirmed(expected, observed);
        return true;
    }

    let observed_name = observed.to_string();
    let identity = check_patch_identity(Some(expected), Some(&observed_name), |name| MetaData::identity_of(expected, name));

    match &identity {
        PatchIdentity::Confirmed => note_confirmed(expected, &observed_name),
        PatchIdentity::Mismatch { .. } => note_mismatch(&identity),
        PatchIdentity::Unconfirmed { expected, observed } => note_unconfirmed(expected, observed),
        PatchIdentity::Unrecorded => {}
    }

    identity.may_apply()
}

/// C10 applied marker: the assets a patch handler has already written into.
///
/// A patch handler is reached twice for the same asset in the ordinary case, not an edge case:
/// `LoadAsset_Internal` and `AssetBundleRequest::GetResult` both end in `on_LoadAsset`, a bundle
/// reloaded after `Unload` is a new object, and one `AnRoot` component is reached through
/// `AnRoot`, `FlashActionPlayer` and `TweenAnimationTimelineComponent`. Writing the same patch
/// twice is the asset version of the compounding multipliers C22 and C24 are: a texture diff
/// composes onto the image it already produced, and `AnRoot` adds a key offset onto an offset it
/// already added. So every handler marks what it wrote and skips what is marked - AGENTS section 5,
/// "Remember the original value ... Never multiply the current value. Use an 'applied' marker when
/// an asset can be processed twice."
///
/// Keyed on the asset address *and* the patch site, every read confirmed through a weak handle to
/// that asset: IL2CPP recycles freed addresses (C8), and a weak handle answers null for a freed
/// asset and a different object for a recycled one, so neither reads as "already patched".
///
/// The site is part of the key because two handlers can legitimately write into the same object from
/// two different files - the texture a `GameObject`'s animation names is the same `Texture2D` asset
/// the texture replacement path picks by load name - and an asset-wide marker would let whichever
/// ran first silently cancel the other. What must not happen twice is the same file written into the
/// same object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PatchSite {
    /// `Texture2D::on_LoadAsset` writing a replacement or a diff for the asset's own path.
    TextureReplacement,
    /// `AtlasReference::on_LoadAsset` writing the texture the atlas's sprites share.
    AtlasTexture,
    /// `AnRoot` writing a texture named by one of its mesh parameter groups.
    AnRootTexture,
    /// `AnRoot` adding a data patch's `anim_pos_offset_adj` to a key parameter list.
    AnRootKeyOffset,
}

static PATCHED_ASSETS: Lazy<Mutex<FnvHashMap<(usize, PatchSite), GCHandle>>> = Lazy::new(|| Mutex::default());

/// One entry per asset and patch site, and only for assets still alive. Past the bound the
/// dead entries are dropped and, if that is not enough, the marker starts over: a missed marker
/// costs one repeat patch, an unbounded marker costs the run.
const PATCHED_ASSETS_BOUND: usize = 4096;

/// The C8 rule for a marker read: an entry counts only when the weak handle behind it still
/// points at the very object the caller is asking about.
#[inline]
fn marker_is_for(marker: *mut Il2CppObject, asset: *mut Il2CppObject) -> bool {
    !marker.is_null() && marker == asset
}

pub fn asset_is_patched(asset: *mut Il2CppObject, site: PatchSite) -> bool {
    if asset.is_null() {
        return false;
    }

    let patched = PATCHED_ASSETS.lock().unwrap_or_else(|e| e.into_inner());
    patched.get(&(asset as usize, site)).is_some_and(|handle| marker_is_for(handle.target(), asset))
}

/// Say that this patch site has written into this asset. Call it when a write happened, not when a
/// handler merely ran: a handler that found nothing to write must leave the asset open for a path
/// that does have something for it.
pub fn mark_asset_patched(asset: *mut Il2CppObject, site: PatchSite) {
    if asset.is_null() {
        return;
    }

    let mut patched = PATCHED_ASSETS.lock().unwrap_or_else(|e| e.into_inner());
    if patched.len() >= PATCHED_ASSETS_BOUND {
        patched.retain(|_, handle| !handle.target().is_null());
        if patched.len() >= PATCHED_ASSETS_BOUND {
            patched.clear();
        }
    }
    patched.insert((asset as usize, site), GCHandle::new_weak_ref(asset, false));
}

type LoadAssetFn = extern "C" fn(this: *mut Il2CppObject, name: *mut Il2CppString, type_: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    LoadAsset_Internal(this: *mut Il2CppObject, name: *mut Il2CppString, type_: *mut Il2CppObject) -> *mut Il2CppObject {
            let asset = get_orig_fn!(LoadAsset_Internal, LoadAssetFn)(this, name, type_);
        on_LoadAsset(this, asset, name);
        asset
    }
}

pub fn LoadAsset_Internal_orig(this: *mut Il2CppObject, name: *mut Il2CppString, type_: *mut Il2CppObject) -> *mut Il2CppObject {
    // Mod code reaching the game's `LoadAsset_Internal` without going through the detour, so an
    // uninstalled hook is reachable here. A null asset is what every caller of a bundle load
    // already handles; a call through 0 on the thread that loads assets is not (C1).
    let Some(load_asset) = get_orig_fn_guarded!(LoadAsset_Internal, LoadAssetFn) else {
        return std::ptr::null_mut();
    };

    load_asset(this, name, type_)
}

type LoadAssetAsyncFn = extern "C" fn(this: *mut Il2CppObject, name: *mut Il2CppString, type_: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    LoadAssetAsync_Internal(this: *mut Il2CppObject, name: *mut Il2CppString, type_: *mut Il2CppObject) -> *mut Il2CppObject {
            let request = get_orig_fn!(LoadAssetAsync_Internal, LoadAssetAsyncFn)(this, name, type_);

        // C9: an async load the game did not start answers null, and that is the answer the caller
        // of the load expects. Registering it would key a live GCHandle on address 0, which
        // `AssetBundleRequest::GetResult` later hands to `on_LoadAsset` as a bundle and a name.
        if request.is_null() || name.is_null() {
            return request;
        }

        let info = RequestInfo {
            name_handle: GCHandle::new(name as _, false), // is name even guaranteed to survive in memory..?
            bundle_handle: GCHandle::new_weak_ref(this as _, false)
        };
        recover_lock(&REQUEST_INFOS).insert(request as usize, info);
        request
    }
}

type OnLoadAssetFn = fn(bundle: *mut Il2CppObject, asset: *mut Il2CppObject, name: &Utf16Str);
pub fn on_LoadAsset(bundle: *mut Il2CppObject, asset: *mut Il2CppObject, name: *mut Il2CppString) {
    // C9 (the site the ledger names): `asset` is what the game's own `LoadAsset_Internal` or
    // `AssetBundleRequest.result` just handed back, and null is what a bundle answers with when it
    // holds no asset of that name - a real answer, not a failure, and one every caller of a load
    // already copes with. `class` is read out of the asset, and `name` out of the string the load
    // was asked for; neither is ours.
    if asset.is_null() || name.is_null() {
        return;
    }

    let class = unsafe { (*asset).klass() };
    if class.is_null() {
        return;
    }
    //debug!("{} {}", unsafe { std::ffi::CStr::from_ptr((*class).name).to_str().unwrap() }, unsafe { (*name).as_utf16str() });

    let handler: OnLoadAssetFn = if class == GameObject::class() {
        GameObject::on_LoadAsset
    }
    else if class == StoryTimelineData::class() {
        StoryTimelineData::on_LoadAsset
    }
    else if class == Texture2D::class() {
        Texture2D::on_LoadAsset
    }
    else if class == AtlasReference::class() {
        AtlasReference::on_LoadAsset
    }
    else if class == StoryRaceTextAsset::class() {
        StoryRaceTextAsset::on_LoadAsset
    }
    else if class == TextRubyData::class() {
        TextRubyData::on_LoadAsset
    }
    else if class == TextDotData::class() {
        TextDotData::on_LoadAsset
    }
    else if class == StoryParamChangeEffect::class() {
        StoryParamChangeEffect::on_LoadAsset
    }
    else {
        return;
    };

    handler(bundle, asset, unsafe { (*name).as_utf16str() });
}

type LoadFromFileInternalFn = extern "C" fn(path: *mut Il2CppString, crc: u32, offset: u64) -> *mut Il2CppObject;
def_detour! {
    LoadFromFile_Internal(path: *mut Il2CppString, crc: u32, offset: u64) -> *mut Il2CppObject {
            get_orig_fn!(LoadFromFile_Internal, LoadFromFileInternalFn)(path, crc, offset)
    }
    bail {
                get_orig_fn!(LoadFromFile_Internal, LoadFromFileInternalFn)(path, crc, offset)
    }
}

pub fn LoadFromFile_Internal_orig(path: *mut Il2CppString, crc: u32, offset: u64) -> *mut Il2CppObject {
    // This used to call the `LoadFromFile_Internal` detour from Rust, which put a helper from
    // `il2cpp::ext` on the same address the hook's trampoline hands its own body - and the hook is
    // not installed unless `init` resolved it. Ask for the original directly, through the same
    // cache, and stay inert when it is not there: a bundle that does not load is what both
    // `ext.rs` call sites already handle (C1).
    let Some(load_from_file) = get_orig_fn_guarded!(LoadFromFile_Internal, LoadFromFileInternalFn) else {
        return std::ptr::null_mut();
    };

    load_from_file(path, crc, offset)
}

def_method_wrapper_fn!(LoadFromMemoryAsync_Internal, LOADFROMMEMORYASYNC_ADDR, *mut Il2CppObject, binary: *mut Il2CppObject, crc: u32);

pub fn load_from_memory_async(binary: &[u8]) -> (*mut Il2CppObject, *mut Il2CppObject) {
    let array = il2cpp_array_new(Byte::class(), binary.len());
    unsafe {
        std::ptr::copy_nonoverlapping(
            binary.as_ptr(),
            (array as *mut u8).offset(0x20),
            binary.len()
        );
    }
    let request = LoadFromMemoryAsync_Internal(array as *mut Il2CppObject, 0);
    (array as *mut Il2CppObject, request)
}

pub fn init(_UnityEngine_AssetBundleModule: *const Il2CppImage) {
    //get_class_or_return!(UnityEngine_AssetBundleModule, UnityEngine, AssetBundle);
    unsafe {
        LOADFROMMEMORYASYNC_ADDR = il2cpp_resolve_icall(
            c"UnityEngine.AssetBundle::LoadFromMemoryAsync_Internal(System.Byte[],System.UInt32)".as_ptr()
        );
    }

    let LoadAsset_Internal_addr = il2cpp_resolve_icall(
        c"UnityEngine.AssetBundle::LoadAsset_Internal(System.String,System.Type)".as_ptr()
    );
    let LoadAssetAsync_Internal_addr = il2cpp_resolve_icall(
        c"UnityEngine.AssetBundle::LoadAssetAsync_Internal(System.String,System.Type)".as_ptr()
    );
    let LoadFromFile_Internal_addr = il2cpp_resolve_icall(
        c"UnityEngine.AssetBundle::LoadFromFile_Internal(System.String,System.UInt32,System.UInt64)".as_ptr()
    );

    new_hook!(LoadAsset_Internal_addr, LoadAsset_Internal);
    new_hook!(LoadAssetAsync_Internal_addr, LoadAssetAsync_Internal);
    new_hook!(LoadFromFile_Internal_addr, LoadFromFile_Internal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use widestring::Utf16String;

    /// The shapes this comparison actually sees, measured rather than invented: the identity a shipped
    /// package records (`UmaTL` English, `assets/atlas/common/common.json` ->
    /// `{"windows":{"bundle_name":"ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"}}`), and the two names a Global
    /// install has for a bundle - the id itself, and the `<Persistent>/dat/<2 chars>/<id>` path it was
    /// loaded from. Neither carries an extension.
    const RECORDED_ID: &str = "ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD";
    const ID_PATH: &str = "C:/Games/Uma/UmamusumePrettyDerby_Data/Persistent/dat/ZV/ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD";

    fn no_meta_table(_: &str) -> MetaIdentity {
        MetaIdentity::Unknown
    }

    /// The confirmation that fires on this client: the recorded id and the name the bundle carries are
    /// the same string, with no `meta` table anywhere in the path.
    #[test]
    fn a_recorded_bundle_id_is_confirmed_by_the_bundles_own_name() {
        assert_eq!(check_patch_identity(Some(RECORDED_ID), Some(RECORDED_ID), no_meta_table), PatchIdentity::Confirmed);
        assert_eq!(check_patch_identity(Some(RECORDED_ID), Some(ID_PATH), no_meta_table), PatchIdentity::Confirmed);
        assert!(check_patch_identity(Some(RECORDED_ID), Some(ID_PATH), no_meta_table).may_apply());
    }

    /// The name comparison the detour runs (on the game's UTF-16 name, in place) and the name
    /// comparison the pure decision runs (on a `&str`) are two implementations of one rule. If they
    /// drift, the guard confirms something the decision would not, or the reverse.
    #[test]
    fn the_two_name_halves_agree_on_the_same_forms() {
        let cases = [
            (RECORDED_ID, ID_PATH),
            (RECORDED_ID, RECORDED_ID),
            ("atlas_common", "res/ui/atlas/atlas_common"),
            ("atlas_common", "res/ui/atlas/atlas_common.a"),
            ("atlas_common.a", "res/ui/atlas/atlas_common.a"),
            ("common", "res/ui/atlas/atlas_common"),
            ("atlas_common_v2", "res/ui/atlas/atlas_common"),
            ("atlas_common", "ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"),
        ];

        for (expected, observed) in cases {
            let on_game_name = bundle_names_match(expected, &Utf16String::from_str(observed));
            let on_decision = check_patch_identity(Some(expected), Some(observed), no_meta_table) == PatchIdentity::Confirmed;
            assert_eq!(on_game_name, on_decision, "the two name halves disagree on {expected} against {observed}");
        }
    }

    /// C10, the case that must not refuse: the identity a package records is the id the *authoring*
    /// install has for that bundle, and this client's id for the same bundle is a different id - which
    /// is also what a bundle whose contents changed looks like from here. `7ae4577` applied these
    /// patches, the first cut of this guard refused them, and an atlas replacement silently stopped
    /// applying for the users the patch was written for.
    #[test]
    fn a_recorded_identity_this_client_has_no_row_for_still_applies() {
        let identity = check_patch_identity(Some("2d8f1a3b9c"), Some("res/ui/atlas/atlas_common"), no_meta_table);
        assert_eq!(identity, PatchIdentity::Unconfirmed {
            expected: "2d8f1a3b9c".to_owned(),
            observed: "res/ui/atlas/atlas_common".to_owned(),
        });
        assert!(identity.may_apply(), "an identity this client cannot resolve is not evidence of a wrong bundle");
    }

    /// A table that says the two ids are the same bundle under different ids answers the same way
    /// (`sql::MetaData::compare_identity`), so the guard's refusal half never sees it.
    #[test]
    fn a_recorded_identity_that_only_resolves_to_the_same_bundle_is_not_a_mismatch() {
        let same_bundle = |_: &str| MetaIdentity::Unknown;
        assert!(check_patch_identity(Some("2d8f1a3b9c"), Some(RECORDED_ID), same_bundle).may_apply());
    }

    /// The #56 jumbled-atlas case, caught on knowledge: this client's own table names the bundle the
    /// patch was authored for and the bundle the asset actually came from as two different bundles.
    #[test]
    fn a_bundle_this_client_names_as_a_different_bundle_is_refused() {
        let table = |_: &str| MetaIdentity::Different {
            expected_logical: "atlas/common_v2".to_owned(),
            observed_logical: "atlas/common".to_owned(),
        };

        let identity = check_patch_identity(Some("222H4K4VZG6BTBWHDB6BVUJHW5LLNT7E"), Some("atlas/common"), table);
        assert_eq!(identity, PatchIdentity::Mismatch {
            expected: "222H4K4VZG6BTBWHDB6BVUJHW5LLNT7E".to_owned(),
            expected_logical: "atlas/common_v2".to_owned(),
            observed: "atlas/common".to_owned(),
            observed_logical: "atlas/common".to_owned(),
        });
        assert!(!identity.may_apply(), "a bundle this client can name as a different one is a refusal");
    }

    /// A table that maps the bundle being loaded to exactly the recorded id confirms it, which is the
    /// Japan shaped case the guard had before and keeps.
    #[test]
    fn a_recorded_id_this_client_records_for_the_same_bundle_is_confirmed() {
        assert_eq!(check_patch_identity(Some(RECORDED_ID), Some("atlas/common"), |_| MetaIdentity::Same), PatchIdentity::Confirmed);
    }

    /// A patch with nothing recorded for this platform is the per-platform opt-out upstream #56
    /// describes: the guard confirms nothing and refuses nothing.
    #[test]
    fn an_unrecorded_identity_neither_confirms_nor_refuses() {
        for observed in [Some("res/ui/atlas/atlas_common"), None] {
            let identity = check_patch_identity(None, observed, no_meta_table);
            assert_eq!(identity, PatchIdentity::Unrecorded);
            assert!(identity.may_apply());
        }
    }

    /// A bundle that names nothing is not evidence about anything, so it is applied unconfirmed and
    /// reported, not refused.
    #[test]
    fn an_unnamed_bundle_is_applied_unconfirmed() {
        let identity = check_patch_identity(Some(RECORDED_ID), None, no_meta_table);
        assert_eq!(identity, PatchIdentity::Unconfirmed {
            expected: RECORDED_ID.to_owned(),
            observed: NO_BUNDLE_NAME.to_owned(),
        });
        assert!(identity.may_apply());
    }

    /// AGENTS section 6: a path walked per asset load says something a bounded number of times, and
    /// the lines that were written still carry the running total rather than pretending it stopped at
    /// the print limit.
    #[test]
    fn an_identity_outcome_is_reported_a_bounded_number_of_times() {
        let counter = AtomicUsize::new(0);
        let printed: Vec<usize> = (0..500).filter_map(|_| within_identity_log_limit(&counter)).collect();

        assert_eq!(printed.len(), IDENTITY_LOG_LIMIT);
        assert_eq!(printed.first(), Some(&1));
        assert_eq!(printed.last(), Some(&20));
        assert_eq!(counter.load(Ordering::Relaxed), 500, "counting does not stop where printing does");
    }

    /// C8: the applied marker only counts while the weak handle behind the entry still names the
    /// same asset. A freed asset (null target) and a recycled address (a different object) both
    /// have to miss, or a reused address skips a patch its new asset needs.
    #[test]
    fn an_applied_marker_only_counts_for_the_asset_it_was_written_for() {
        let asset = 0x1234 as *mut Il2CppObject;
        let recycled_by_someone_else = 0x5678 as *mut Il2CppObject;

        assert!(marker_is_for(asset, asset));
        assert!(!marker_is_for(std::ptr::null_mut(), asset), "a freed asset must not read as patched");
        assert!(!marker_is_for(recycled_by_someone_else, asset), "a recycled address must not read as patched");
    }
}