pub mod Localize;
pub mod TextId;
pub mod StoryRaceTextAsset;
mod LyricsController;
pub mod StoryTimelineData;
pub mod StoryTimelineBlockData;
pub mod StoryTimelineTrackData;
pub mod StoryTimelineTextClipData;
pub mod GallopUtil;
pub mod UIManager;
pub mod GraphicSettings;
mod CameraController;
pub mod SingleModeStartResultCharaViewer;
pub mod WebViewManager;
pub mod DialogCommon;
mod PartsSingleModeSkillLearningListItem;
mod TrainingParamChangeA2U;
pub mod WebViewDefine;
pub mod TextFrame;
pub mod PartsSingleModeSkillListItem;
pub mod FlashActionPlayer;
pub mod TextRubyData;
pub mod TextDotData;
pub mod GameSystem;
pub mod StoryViewTextControllerBase;
mod StoryViewTextControllerLandscape;
mod StoryViewTextControllerSingleMode;
mod JikkyoDisplay;
pub mod Screen;
#[cfg(target_os = "windows")]
pub mod LandscapeUIManager;
#[cfg(target_os = "windows")]
pub mod StandaloneWindowResize;
#[cfg(target_os = "windows")]
mod GallopInput;
#[cfg(target_os = "windows")]
mod InputSystemManager;
#[cfg(target_os = "windows")]
mod BackKeyInputManager;
#[cfg(target_os = "windows")]
pub mod WindowsGamepadControl;
pub mod TapEffectController;
mod TrainingParamChangePlate;
mod SingleModeUtils;
mod MasterSingleModeTurn;
mod TextFontManager;
mod TextFormat;
pub mod TextCommon;
mod TextMeshProUguiCommon;
mod StoryChoiceController;
mod StoryViewController;
mod StoryTimelineClipData;
mod StoryTimelineCharaTrackData;
mod CharacterNoteTopView;
mod CharacterNoteTopViewController;
mod ViewControllerBase;
mod ButtonCommon;
mod NowLoading;
pub mod StoryTimelineController;
mod DialogRaceOrientation;
pub mod RaceDefine;
pub mod RaceInfo;
pub mod RacePhaseCalculator;
mod RaceUtil;
mod SaveDataManager;
mod ApplicationSettingSaveLoader;
mod HighSpeedSetting;
mod LiveTheaterCharaSelect;
mod LiveTheaterViewController;
pub mod CySpringController;
mod LiveUtil;
pub mod MasterDataUtil;
pub mod DialogCommonBase;
pub mod DialogObject;
pub mod AudioManager;
pub mod MasterCharacterSystemText;
pub mod ImageCommon;
pub mod Notification;
mod TimeUtil;
pub mod CameraData;
pub mod CascadeShadow;
mod CascadeShadowForRace;
pub mod GallopRenderer;
pub mod StoryTimelineBg3DClipData;
pub mod DialogManager;
pub mod PartsCharaMessageBase;
pub mod SceneManager;
pub mod AnimationSpeed;
pub mod SingleModeResultContentBase;
mod StoryFrameProbe;
mod TrainingCuttProbe;
// The wait doors in `UnityEngine_CoreModule::WaitProbe` are outside this assembly's module tree, and one
// function is the only thing they need: a hole census that cannot see the yields the game armed inside it
// would report a 12 s wait as an unexplained gap. The module itself stays private.
pub(crate) use TrainingCuttProbe::note_hole_census_value;
mod CutStateProbe;
mod StoryEventProbe;
mod GameFrameProbe;
mod LowResolutionCamera;

#[cfg(target_os = "windows")]
mod PaymentUtility;
#[cfg(target_os = "windows")]
mod LiveTimelineControl;
#[cfg(target_os = "windows")]
pub mod LiveTimelineWorkSheet;
#[cfg(target_os = "windows")]
pub mod LiveTimelineKeyPostFilmDataList;
#[cfg(target_os = "windows")]
pub mod LiveTimelineKeyCameraPositionData;
#[cfg(target_os = "windows")]
mod LiveTimelineKeyCameraLookAtData;
#[cfg(target_os = "windows")]
mod LiveTimelineKeyMultiCameraPositionData;
#[cfg(target_os = "windows")]
mod CharacterObject;
#[cfg(target_os = "windows")]
mod LiveModelController;
#[cfg(target_os = "windows")]
pub mod ModelController;
#[cfg(target_os = "windows")]
mod RaceCameraManager;
#[cfg(target_os = "windows")]
mod RaceCameraEventBase;
#[cfg(target_os = "windows")]
mod RaceModelController;
#[cfg(target_os = "windows")]
mod RaceViewBase;
#[cfg(target_os = "windows")]
mod RaceEffectManager;
pub mod HorseData;
pub mod HorseRaceInfo;
pub mod JikkyoControllerBase;
pub mod Jikkyo;
pub mod RaceBGMController;
pub mod RaceMainViewController;
pub mod RaceManager;
pub mod RaceManagerReplayBase;
pub mod RaceEventPlayer;
pub mod RaceHorseManagerBase;
pub mod RaceSoundReplay;
pub mod RaceUI;
pub mod RaceUIMiniMap;
pub mod RaceViewReplay;
pub mod RaceSimulateData;
pub mod RaceSimulateEventData;
pub mod RaceSimulateReader;
pub mod RaceHorseManagerReplay;
pub mod RaceSimulateFrameData;
pub mod RaceSimulateHorseFrameData;

#[path = "SimulateEventType.rs"]
mod simulate_event_type;
pub use simulate_event_type::SimulateEventType;
#[path = "TemptationMode.rs"]
mod temptation_mode;
pub use temptation_mode::TemptationMode;

pub mod SkillManager;
pub mod SkillBase;
pub mod HorseRaceInfoReplay;
#[cfg(target_os = "windows")]
mod PartsScheduleBookAutoPlayScreen;
pub mod TweenAnimationTimelineComponent;
pub mod TweenAnimationTimelineData;
pub mod TweenAnimationTimelineSheetData;
mod PartsSingleModeChoiceRewardTextElementViewModel;
mod PartsCommonHeaderTitle;
pub mod StoryParamChangeEffect;
mod PartsRaceAnalyzeRaceEventListItem;
pub mod PartsNickNameRibbon;
mod PartsNickNameListItem;
mod PartsGetSkillPlate;
mod StoryChoiceButton;
mod DialogMissionListItem;
mod PartsNamePlateBase;
mod PartsSupportCardImproveDetail;
#[cfg(target_os = "windows")]
mod Connecting;
#[cfg(target_os = "windows")]
mod DownloadManager;
#[cfg(target_os = "windows")]
mod DownloadView;
#[cfg(target_os = "windows")]
mod DownloadErrorProcessor;
#[cfg(target_os = "windows")]
mod TitleViewController;
#[cfg(target_os = "windows")]
pub mod MainGameInitializer;
pub mod Director;
mod CySpringNative;
pub mod LiveViewController;
pub mod LiveTimeController;
pub mod HomeViewController;
pub mod WorkDataManager;
pub mod AssetManager;
pub mod WorkJukeboxData;
pub mod JukeboxBgmSelector;
pub mod JukeboxHomeTopUI;
pub mod TempData;
pub mod MasterJukeboxSetlistMusicData;
pub mod HubViewControllerBase;
mod LiveTheaterInfo;
pub mod DownloadPathRegister;
pub mod SceneDefine;
pub mod GameDefine;
pub mod MasterDataManager;
pub mod MasterItemExchangeTop;

pub fn init() {
    get_assembly_image_or_return!(image, "umamusume.dll");

    Localize::init(image);
    TextId::init(image);
    StoryRaceTextAsset::init(image);
    LyricsController::init(image);
    StoryTimelineData::init(image);
    StoryTimelineBlockData::init(image);
    StoryTimelineTrackData::init(image);
    StoryTimelineTextClipData::init(image);
    StoryTimelineBg3DClipData::init(image);
    GallopUtil::init(image);
    UIManager::init(image);
    GraphicSettings::init(image);
    CameraController::init(image);
    SingleModeStartResultCharaViewer::init(image);
    WebViewManager::init(image);
    DialogCommon::init(image);
    PartsSingleModeSkillLearningListItem::init(image);
    TrainingParamChangeA2U::init(image);
    TextFrame::init(image);
    PartsSingleModeSkillListItem::init(image);
    FlashActionPlayer::init(image);
    TextRubyData::init(image);
    TextDotData::init(image);
    GameSystem::init(image);
    StoryViewTextControllerBase::init(image);
    StoryViewTextControllerLandscape::init(image);
    StoryViewTextControllerSingleMode::init(image);
    JikkyoDisplay::init(image);
    Screen::init(image);
    TrainingParamChangePlate::init(image);
    SingleModeUtils::init(image);
    MasterSingleModeTurn::init(image);
    TextFontManager::init(image);
    TextFormat::init(image);
    TextCommon::init(image);
    TextMeshProUguiCommon::init(image);
    StoryChoiceController::init(image);
    StoryViewController::init(image);
    StoryTimelineClipData::init(image);
    StoryTimelineCharaTrackData::init(image);
    CharacterNoteTopView::init(image);
    CharacterNoteTopViewController::init(image);
    ViewControllerBase::init(image);
    ButtonCommon::init(image);
    NowLoading::init(image);
    StoryTimelineController::init(image);
    DialogRaceOrientation::init(image);
    RaceInfo::init(image);
    RacePhaseCalculator::init(image);
    RaceUtil::init(image);
    SaveDataManager::init(image);
    ApplicationSettingSaveLoader::init(image);
    HighSpeedSetting::init(image);
    LiveTheaterCharaSelect::init(image);
    LiveTheaterViewController::init(image);
    CySpringController::init(image);
    LiveUtil::init(image);
    MasterDataUtil::init(image);
    DialogCommonBase::init(image);
    DialogObject::init(image);
    AudioManager::init(image);
    MasterCharacterSystemText::init(image);
    ImageCommon::init(image);
    Notification::init(image);
    TimeUtil::init(image);
    DialogManager::init(image);
    PartsCharaMessageBase::init(image);
    SceneManager::init(image);
    LowResolutionCamera::init(image);
    TapEffectController::init(image);

    #[cfg(target_os = "windows")]
    {
        LandscapeUIManager::init(image);
        StandaloneWindowResize::init(image);
        GallopInput::init(image);
        InputSystemManager::init(image);
        BackKeyInputManager::init(image);
        WindowsGamepadControl::init(image);
        PaymentUtility::init(image);
        Connecting::init(image);
        DownloadManager::init(image);
        DownloadView::init(image);
        DownloadErrorProcessor::init(image);
        MainGameInitializer::init(image);
        LiveTimelineControl::init(image);
        LiveTimelineWorkSheet::init(image);
        LiveTimelineKeyPostFilmDataList::init(image);
        LiveTimelineKeyCameraPositionData::init(image);
        LiveTimelineKeyCameraLookAtData::init(image);
        LiveTimelineKeyMultiCameraPositionData::init(image);
        CharacterObject::init(image);
        LiveModelController::init(image);
        ModelController::init(image);
        RaceCameraManager::init(image);
        RaceCameraEventBase::init(image);
        RaceModelController::init(image);
        RaceViewBase::init(image);
        RaceEffectManager::init(image);
        TitleViewController::init(image);
        PartsScheduleBookAutoPlayScreen::init(image);
    }
    HorseData::init(image);
    HorseRaceInfo::init(image);
    JikkyoControllerBase::init(image);
    Jikkyo::init(image);
    RaceBGMController::init(image);
    RaceMainViewController::init(image);
    RaceManager::init(image);
    RaceManagerReplayBase::init(image);
    RaceEventPlayer::init(image);
    RaceSoundReplay::init(image);
    RaceUI::init(image);
    RaceUIMiniMap::init(image);
    RaceViewReplay::init(image);
    RaceHorseManagerBase::init(image);
    RaceSimulateData::init(image);
    RaceSimulateEventData::init(image);
    RaceSimulateReader::init(image);
    RaceHorseManagerReplay::init(image);
    RaceSimulateFrameData::init(image);
    RaceSimulateHorseFrameData::init(image);
    HorseRaceInfoReplay::init(image);
    SkillManager::init(image);
    SkillBase::init(image);
    CameraData::init(image);
    CascadeShadow::init(image);
    CascadeShadowForRace::init(image);
    GallopRenderer::init(image);
    TweenAnimationTimelineComponent::init(image);
    TweenAnimationTimelineData::init(image);
    TweenAnimationTimelineSheetData::init(image);
    PartsSingleModeChoiceRewardTextElementViewModel::init(image);
    PartsCommonHeaderTitle::init(image);
    StoryParamChangeEffect::init(image);
    PartsRaceAnalyzeRaceEventListItem::init(image);
    PartsNickNameRibbon::init(image);
    PartsNickNameListItem::init(image);
    PartsGetSkillPlate::init(image);
    StoryChoiceButton::init(image);
    DialogMissionListItem::init(image);
    PartsNamePlateBase::init(image);
    PartsSupportCardImproveDetail::init(image);
    Director::init(image);
    CySpringNative::init(image);
    LiveViewController::init(image);
    LiveTimeController::init(image);
    HomeViewController::init(image);
    WorkDataManager::init(image);
    AssetManager::init(image);
    WorkJukeboxData::init(image);
    JukeboxBgmSelector::init(image);
    JukeboxHomeTopUI::init(image);
    TempData::init(image);
    MasterJukeboxSetlistMusicData::init(image);
    HubViewControllerBase::init(image);
    LiveTheaterInfo::init(image);
    DownloadPathRegister::init(image);
    MasterDataManager::init(image);
    MasterItemExchangeTop::init(image);

    // Resolved last: the duration constants are read straight out of the loaded
    // metadata, and every module above may still be filling in class lookups.
    AnimationSpeed::init(image);
    SingleModeResultContentBase::init(image);

    // Diagnostic only, and only when debug_mode is on: records which frame stepping paths this
    // client really calls. It has to run after the scaling modules so it observes the same class
    // lookups they resolved.
    StoryFrameProbe::init(image);

    // Also diagnostic only: the same shape of measurement for the training screen's cut-in, so a
    // friendship training animation has a number in front of any decision to speed it up.
    TrainingCuttProbe::init(image);

    // And the same again one level down, on the coroutine the training turn waits on: the values the game
    // computed for the cut, and the branch of that coroutine the game sits in while the turn takes its
    // time. Observe only, and installed after the door it reads the coroutine object from.
    CutStateProbe::init(image);

    // And for the cut-in a story or story event screen drops into its text, which runs through
    // `CutInHelper` and the static extension doors rather than the training cut controller.
    StoryEventProbe::init(image);
    // No hooks, only the switch the frame clock in GameSystem_Update reads.
    GameFrameProbe::init();
}
