use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use crate::{
    core::{Hachimi, gui::{GameOpts, GAME_OPTS_CACHE}, game::Region},
    il2cpp::{
        sql::{get_champions_resources, get_champions_live_max_year},
        symbols::{IEnumerator, MoveNextFn, SingletonLike, get_method_addr},
        types::*, utils::umamusume_enum_options
    }
};
#[cfg(target_os = "windows")]
use crate::windows::free_camera::{self, CameraScene};
#[cfg(target_os = "windows")]
use crate::core::live_utils;
#[cfg(target_os = "windows")]
use super::Director;
use super::GraphicSettings::{self, MsaaQuality};

// C16: the initialization is reached from two independent paths and nothing recorded that it had
// already happened. The first is the eager pass at the end of hook arming
// (`core::hachimi::on_hooking_finished`), which sits there so the values it writes precede game
// code. The second is this file's `InitializeGame_MoveNext`, which fires when the game's own
// `GameSystem.InitializeGame` coroutine answers `false`; `InitializeGameCommon` only arms it when
// `ui_scale != 1.0`, which is exactly when the ledger says the pass runs twice. And because
// `hook_move_next` stands on the compiler generated enumerator class and is never taken down (C15),
// every later `InitializeGame` of that class - a re-init, a soft reset - arrives here too.
//
// One latch, claimed by whoever arrives first. A second pass was not harmless: it re-ran the game
// options SQL walk (`init_game_opts`), fired every plugin callback twice, and re-entered the config
// mirrors and the apply passes over values the first pass had already written.
static GAME_INITIALIZED: AtomicBool = AtomicBool::new(false);

// The refused arrival is the event a run has to see in hachimi.log to show the latch held (AGENTS
// section 7), so it is told once per session instead of once per arrival.
static LATER_ARRIVAL_WARNED: AtomicBool = AtomicBool::new(false);

/// The latch decision `on_game_initialized()` gates its whole body on: `true` for the one arrival
/// that owns the initialization, `false` for every arrival after it. Kept apart from the body
/// because the body reaches `Hachimi::instance()`, which ends the process when it is asked for
/// before the singleton exists - so the decision, not the body, is what a unit test can drive.
fn claim_game_initialized() -> bool {
    !GAME_INITIALIZED.swap(true, Ordering::AcqRel)
}

static mut CLASS: *mut Il2CppClass = 0 as _;
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

pub fn instance() -> *mut Il2CppObject {
    let Some(singleton) = SingletonLike::new(class()) else {
        return 0 as _;
    };
    singleton.instance()
}

static mut SOFTWARERESET_ADDR: usize = 0;
impl_addr_wrapper_fn!(SoftwareReset, SOFTWARERESET_ADDR, (), this: *mut Il2CppObject);

type GameSystemUpdateFn = extern "C" fn(this: *mut Il2CppObject);
#[cfg(target_os = "windows")]
fn apply_free_camera_live_pause_request() {
    if !free_camera::take_toggle_live_pause_request() {
        return;
    }
    live_utils::toggle_live_pause();
}

def_detour! {
    GameSystem_Update(this: *mut Il2CppObject) {
            // First thing in the detour, so the gap measured is the gap between two of the game's own ticks and
        // not the gap this probe spent working inside one.
        super::GameFrameProbe::observe_frame();
        crate::core::gui::race_slider_drain();

        // C2 barrier item 2: a coroutine door that tripped took itself out of the hook registry from
        // inside its own frame (`guard::coroutine_trip`), and left the backend half of that - putting this
        // method's bytes back and letting its trampoline go - for a point that is not a frame of the door
        // being taken down. This is that point: the same game-thread tick this file already writes on, and
        // a point at which the frame that asked has certainly returned. Only door take-downs are ever
        // waiting here - `Interceptor::unhook` runs a hook on anything else where it was asked for - and
        // while nothing is waiting it costs one relaxed load (AGENTS section 6).
        Hachimi::instance().interceptor.drain_deferred_unhooks();

        Hachimi::instance().drain_skill_data_desc_rebuild();
        crate::il2cpp::hook::UnityEngine_CoreModule::Time::apply_if_dirty();
        crate::il2cpp::hook::umamusume::AnimationSpeed::apply_if_dirty();
        crate::il2cpp::hook::umamusume::StoryTimelineController::engage_high_speed_mode();
        super::StoryFrameProbe::report_if_due();
        // The training cut-in measurement reports on the same tick and on the same quiet path rule.
        super::TrainingCuttProbe::report_if_due();
        // What the training cut's own coroutine was waiting on, and how long the game armed each coroutine
        // wait for, on the same rule.
        super::CutStateProbe::report_if_due();
        crate::il2cpp::hook::UnityEngine_CoreModule::WaitProbe::report_if_due();
        super::StoryEventProbe::report_if_due();
        super::GameFrameProbe::report_if_due();

        #[cfg(target_os = "windows")]
        {
            apply_free_camera_live_pause_request();

            // Live and race normally tick from their camera LateUpdate hooks. Keep the
            // global update path only as a fallback while LiveTimelineControl is paused.
            if Director::is_live_paused() && free_camera::scene() == CameraScene::Live {
                free_camera::tick();
                apply_free_camera_live_pause_request();
            }
        }

        get_orig_fn!(GameSystem_Update, GameSystemUpdateFn)(this);
    }
}

#[cfg(target_os = "windows")]
type GameSystemLateUpdateFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    #[cfg(target_os = "windows")]
    GameSystem_LateUpdate(this: *mut Il2CppObject) {
            get_orig_fn!(GameSystem_LateUpdate, GameSystemLateUpdateFn)(this);
        Director::apply_paused_free_camera();
    }
}

fn init_game_opts() {
    let opts = GameOpts {
        champions_resources: Arc::new(get_champions_resources()),
        champions_live_max_year: get_champions_live_max_year(),
        font_color_options: Arc::new(umamusume_enum_options(c"FontColorType")),
        outline_size_options: Arc::new(umamusume_enum_options(c"OutlineSizeType")),
        outline_color_options: Arc::new(umamusume_enum_options(c"OutlineColorType")),
    };
    match GAME_OPTS_CACHE.lock() {
        Ok(mut slot) => *slot = Some(opts),
        Err(poisoned) => {
            warn!("GAME_OPTS_CACHE mutex poisoned, recovering");
            *poisoned.into_inner() = Some(opts);
        }
    }
}

// good hook for initializing values i guess
pub fn on_game_initialized() {
    // C16 follow-up: the plugin callbacks belong to `Hachimi`, which owns the queue, and they are
    // dispatched here on every arrival - including one the latch below refuses - because a plugin
    // registers its callback during `plugin.init()`, and `core::hachimi::on_hooking_finished` runs
    // that pass *after* the eager call that claims this latch. Refusing an initialization pass must
    // not refuse a callback. `Hachimi` also takes every callback out of the queue and unlocks it
    // before it calls one: this body used to call third-party code while holding the queue lock and
    // reached that lock with `unwrap()`, so a plugin that registered from its own callback locked
    // against itself and a plugin that faulted turned every later call into a panic across FFI.
    Hachimi::instance().dispatch_plugin_init_callbacks();

    // The whole pass is claimed by the latch above, so every value below is written once per
    // session no matter how many paths reach this function.
    if !claim_game_initialized() {
        if !LATER_ARRIVAL_WARNED.swap(true, Ordering::AcqRel) {
            info!("GameSystem: initialization already done, later initialization pass skipped");
        }

        return;
    }

    Hachimi::instance().init_character_data();
    let hachimi = Hachimi::instance();
    hachimi.init_skill_info();
    hachimi.init_skill_data_desc();
    init_game_opts();

    crate::il2cpp::hook::UnityEngine_CoreModule::Time::apply();
    crate::il2cpp::hook::umamusume::AnimationSpeed::apply();

    #[cfg(target_os = "android")]
    crate::android::utils::set_audio_capture_policy_all();
    #[cfg(target_os = "windows")]
    super::UIManager::apply_ui_scale();

    if Hachimi::instance().config.load().msaa != MsaaQuality::Disabled {
        let graphic_settings = GraphicSettings::instance();
        if !graphic_settings.is_null() {
            GraphicSettings::set__isMSAA(graphic_settings, true);
        }
    }
}

def_detour! {
    InitializeGame_MoveNext(enumerator: *mut Il2CppObject) coroutine answer -> bool {
            let moved = get_orig_fn!(InitializeGame_MoveNext, MoveNextFn)(enumerator);
        // The game's own answer is published before any of the mod's work: a trip in
        // `on_game_initialized` is answered with what this coroutine actually said, not with a
        // `false` that tells the game its initialization coroutine completed.
        answer.publish(moved);
        if !moved {
            // Game has finished initializing
            on_game_initialized();
        }
        moved
    }
}

fn InitializeGameCommon(enumerator: IEnumerator) -> IEnumerator {
    if Hachimi::instance().config.load().ui_scale == 1.0 { return enumerator; }

    if let Err(e) = enumerator.hook_move_next(InitializeGame_MoveNext) {
        error!("Failed to hook InitializeGame enumerator: {}", e);
    }

    enumerator
}

type InitializeGameJpFn = extern "C" fn(this: *mut Il2CppObject, on_complete_initialize_ui: *mut Il2CppObject) -> IEnumerator;
def_detour! {
    InitializeGameJp(this: *mut Il2CppObject, on_complete_initialize_ui: *mut Il2CppObject) answer -> IEnumerator {
            let enumerator = get_orig_fn!(InitializeGameJp, InitializeGameJpFn)(this, on_complete_initialize_ui);
        // The enumerator the game produced is published before arming the door: if arming trips,
        // the coroutine the game started is still handed back to it instead of a zero one.
        answer.publish(IEnumerator::from(enumerator.this));
        InitializeGameCommon(enumerator)
    }
}

type InitializeGameOtherFn = extern "C" fn(this: *mut Il2CppObject) -> IEnumerator;
def_detour! {
    InitializeGameOther(this: *mut Il2CppObject) answer -> IEnumerator {
            let enumerator = get_orig_fn!(InitializeGameOther, InitializeGameOtherFn)(this);
        answer.publish(IEnumerator::from(enumerator.this));
        InitializeGameCommon(enumerator)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, GameSystem);

    if Hachimi::instance().game.region == Region::Japan {
        let InitializeGame_addr = get_method_addr(GameSystem, c"InitializeGame", 1);
        new_hook!(InitializeGame_addr, InitializeGameJp);
    }
    else {
        let InitializeGame_addr = get_method_addr(GameSystem, c"InitializeGame", 0);
        new_hook!(InitializeGame_addr, InitializeGameOther);
    }

    unsafe {
        CLASS = GameSystem;
        SOFTWARERESET_ADDR = get_method_addr(GameSystem, c"SoftwareReset", 0);
    }

    let GameSystem_Update_addr = get_method_addr(GameSystem, c"Update", 0);
    new_hook!(GameSystem_Update_addr, GameSystem_Update);
    #[cfg(target_os = "windows")]
    {
        let GameSystem_LateUpdate_addr = get_method_addr(GameSystem, c"LateUpdate", 0);
        new_hook!(GameSystem_LateUpdate_addr, GameSystem_LateUpdate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // C16's two arrivals, run through the gate `on_game_initialized()` puts on its body, counting
    // how many times the body happened. There was no gate before the latch, so both arrivals ran
    // it and the count was 2.
    //
    // The body is a stand-in because the real one reaches `Hachimi::instance()`, which ends the
    // process when a test asks for it (AGENTS section 4); the gate is the shipped one. This is the
    // only test that claims the process wide latch, which is what a latch is: one pass per process,
    // however many arrivals there are.
    #[test]
    fn the_initialization_body_runs_once_across_both_arrivals() {
        static PASS_RUNS: AtomicUsize = AtomicUsize::new(0);

        // The shape of `on_game_initialized`: claim the latch, and only then run the pass.
        fn arrival() {
            if claim_game_initialized() {
                PASS_RUNS.fetch_add(1, Ordering::Relaxed);
            }
        }

        // The eager pass at the end of hook arming (`core::hachimi::on_hooking_finished`) and the
        // game's `InitializeGame` coroutine finishing through `InitializeGame_MoveNext`, raced the
        // way they land on each other in a real session.
        let hooking_finished = std::thread::spawn(arrival);
        let coroutine_finished = std::thread::spawn(arrival);
        hooking_finished.join().unwrap();
        coroutine_finished.join().unwrap();

        assert_eq!(PASS_RUNS.load(Ordering::Relaxed), 1, "both arrivals ran the initialization pass");

        // C15: the coroutine abort stands on the compiler generated enumerator class and is never
        // taken down, so every later `InitializeGame` of that class - a re-init, a soft reset -
        // arrives here too. They are all refused, and a refusal costs one swap rather than a second
        // pass over the game's data.
        for _ in 0..10_000 {
            arrival();
        }

        assert_eq!(PASS_RUNS.load(Ordering::Relaxed), 1, "a later arrival ran the initialization pass");
    }
}
