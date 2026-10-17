#pragma once
#include "MainWindow.g.h"
#include <atomic>
#include <cstddef>
#include <condition_variable>
#include <cstdint>
#include <mutex>
#include <optional>
#include <queue>
#include <string>
#include <thread>
#include <utility>
#include <vector>
#include <winrt/Microsoft.UI.Dispatching.h>
#include <winrt/Microsoft.UI.Xaml.Controls.h>
#include <winrt/Microsoft.UI.Xaml.Input.h>
#include <winrt/Microsoft.UI.Xaml.Shapes.h>
#include <winrt/Windows.Data.Json.h>

namespace winrt::SettingsUI::implementation
{
    struct AppearancePresetPreviewJob
    {
        winrt::Windows::Data::Json::JsonObject changes{ nullptr };
        uint32_t cardIndex{};
    };

    struct MainWindow : MainWindowT<MainWindow>
    {
        MainWindow();
        ~MainWindow();
        void OnNavigationChanged(Microsoft::UI::Xaml::Controls::NavigationView const&,
            Microsoft::UI::Xaml::Controls::NavigationViewSelectionChangedEventArgs const& args);
        void InitializePipe();
        void RenderPage(std::wstring const& page);
        void Receive(winrt::Windows::Data::Json::JsonObject const& message);
        void Send(winrt::Windows::Data::Json::JsonObject request);
        void FlushPending();
        void SendNextMutation();
        void UpdateSaveBar();
        void UpdateToastPosition();
        void DiscardPendingChanges();
        void UpdateStickyPreview();
        void ClearStickyPreview();
        void ContinueDeferredWork();
        void ShowCloseSaveFailureDialog();
        void CompleteCloseSaveDecision(Microsoft::UI::Xaml::Controls::ContentDialogResult result, bool completed);
        void CloseIfOpen();
        void OnWindowClosed();
        void RevokeWindowEvents();
        void SyncPaneState();
        void QueuePaneState(bool open);
        void SendPendingPaneState();
        void SetColorDraft(std::wstring const& key, std::optional<std::wstring> draft);
        void ClearResetColorDraftsIfUnchanged(uint64_t claudeRevision, uint64_t codexRevision, uint64_t antigravityRevision, uint64_t opencodeRevision, uint64_t cursorRevision, uint64_t grokRevision, uint64_t copilotRevision, uint64_t usageTextRevision);
        void CompleteColorApply(std::wstring const& key, std::optional<std::wstring> const& appliedDraft);
        bool HasUnappliedColorDraft() const;
        bool HasPendingChanges() const;
        void ScheduleWidgetPreview();
        void SendWidgetPreviewIfIdle();
        void UpdateWidgetPreview(winrt::Windows::Data::Json::JsonObject const& snapshot);
        winrt::fire_and_forget LoadWidgetPreview(std::wstring imageBase64, uint64_t generation);
        winrt::fire_and_forget LoadPresetPreview(std::wstring imageBase64, uint64_t generation, uint32_t index);
        void ApplyAppearancePreset(winrt::Windows::Data::Json::JsonObject const& target);
        void SendNextPresetPreviewIfIdle();
        void BuildAppearancePresets();
        void OnWindowClosing(Microsoft::UI::Windowing::AppWindow const&,
            Microsoft::UI::Windowing::AppWindowClosingEventArgs const& args);
        void SetValue(std::wstring const& key, winrt::Windows::Data::Json::IJsonValue const& value);
        void Action(std::wstring const& action);
        void ShowMessage(std::wstring const& text, bool error = false);
        void UpdateProviderAvailability();
        void UpdateWidgetPageAvailability();
        void AddWidgetHiddenWarning();
        void AppendWidgetDependentCard(Microsoft::UI::Xaml::Controls::Border const& card);
        void UpdateAboutControls();
        void UpdateUpdateIndicator();
        void UpdateTitleBarLayout();
        void OpenThirdPartyNotices();
        void Hydrate();
        void RefreshSettingsControlsIfSafe();
        void BuildGeneral();
        void BuildMonitoring();
        void BuildAppearance();
        void BeginProviderReorder(Microsoft::UI::Xaml::Controls::Border const& tile,
            Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void MoveProviderReorder(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void EndProviderReorder(Microsoft::UI::Xaml::Input::PointerRoutedEventArgs const& args);
        void UpdateProviderDropIndicator(winrt::Windows::Foundation::Point const& point);
        void CancelProviderReorder();
        void PersistProviderOrder();
        void BuildProviderDetail(int provider);
        void AddAccentColorRow(Microsoft::UI::Xaml::Controls::StackPanel const& parent,
            std::wstring const& key, std::wstring const& title);
        void BuildPlacement();
        void BuildAbout();
        void BuildUpdate();
        void BuildExperimental();
#if QUOTA_DEVELOPMENT_UI
        void OpenDevelopment();
        void BuildDevelopment();
        void QueueDevelopmentRequest(winrt::Windows::Data::Json::JsonObject const& request);
        void SendNextDevelopmentRequest();
#endif
        void NavigateTo(std::wstring const& page);
        void BuildPreviewCard();
        void UpdatePreviewOverflow();
        winrt::Windows::Data::Json::JsonObject Setting(std::wstring const& key);
        bool Bool(std::wstring const& key);
        bool AppearanceProviderEnabled(std::wstring const& key);
        std::wstring Text(std::wstring const& key);
        winrt::Windows::Data::Json::JsonObject State() const;

    private:
        std::wstring pipeName_;
        uint32_t parentPid_{};
        std::wstring page_{ L"General" };
        int detailProvider_{ -1 };
        winrt::Windows::Data::Json::JsonObject state_{ nullptr };
        winrt::Windows::Data::Json::JsonObject flightChanges_{ nullptr };
        winrt::Windows::Data::Json::JsonObject pendingChanges_{ nullptr };
        std::vector<winrt::Microsoft::UI::Xaml::Controls::ToggleSwitch> providerToggles_;
        std::vector<winrt::Microsoft::UI::Xaml::Controls::ContentControl> widgetDependentCards_;
        winrt::Microsoft::UI::Xaml::Controls::ToggleSwitch providerDetailEnableToggle_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::ToggleSwitch widgetVisibilityToggle_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::ComboBox providerIconPositionSelector_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::ComboBox providerSpacingSelector_{ nullptr };
#if QUOTA_DEVELOPMENT_UI
        winrt::Microsoft::UI::Xaml::Controls::NavigationViewItem developmentNavigationItem_{ nullptr };
        std::queue<winrt::Windows::Data::Json::JsonObject> developmentRequests_;
        bool developmentRequestInFlight_{};
        bool developmentResetInFlight_{};
#endif
        winrt::Microsoft::UI::Xaml::Controls::GridView providerOrderGrid_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Canvas providerOrderOverlay_{ nullptr };
        winrt::Microsoft::UI::Xaml::Shapes::Rectangle providerInsertionIndicator_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Border providerDraggedTile_{ nullptr };
        winrt::Microsoft::UI::Xaml::Input::Pointer providerPointer_{ nullptr };
        std::vector<std::wstring> providerFullOrder_;
        std::optional<int> providerDragOriginalIndex_;
        std::optional<int> providerDropIndex_;
        winrt::Windows::Foundation::Point providerPressPoint_{};
        uint32_t providerPointerId_{};
        int providerDragProviderIndex_{ -1 };
        bool providerPointerPressed_{};
        bool providerDragActive_{};
        winrt::Microsoft::UI::Xaml::Controls::Image widgetPreviewImage_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock widgetPreviewStatus_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Border appearancePreviewCard_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Border appearancePreviewPlaceholder_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Border appearanceDetailTitleCard_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Border appearanceDetailTitlePlaceholder_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock previewOverflowWarning_{ nullptr };
        bool appearancePreviewFloating_{};
        bool appearanceTitleFloating_{};
        double appearancePreviewFloatOffset_{};
        winrt::Microsoft::UI::Xaml::Controls::ScrollViewer pageScroll_{ nullptr };
        winrt::event_token pageScrollViewToken_{};
        winrt::Microsoft::UI::Xaml::Controls::TextBlock versionText_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock updateStatusText_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock uninstallDescription_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Button checkForUpdatesButton_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::InfoBadge updateIndicator_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::ContentDialog closeSaveFailureDialog_{ nullptr };
        winrt::Microsoft::UI::Dispatching::DispatcherQueue dispatcher_{ nullptr };
        winrt::Microsoft::UI::Dispatching::DispatcherQueueTimer previewTimer_{ nullptr };
        winrt::Microsoft::UI::Dispatching::DispatcherQueueTimer messageTimer_{ nullptr };
        std::atomic<bool> stopping_{ false };
        std::atomic<bool> hydrating_{ false };
        std::mutex requestsMutex_;
        std::condition_variable requestsChanged_;
        std::queue<std::string> requests_;
        uint32_t nextId_{ 1 };
        uint32_t inFlight_{ 0 };
        uint64_t revision_{};
        bool initialGetPending_{};
        bool initialStateReady_{};
        bool mutationInFlight_{};
        bool persistInFlight_{};
        bool persistNeeded_{};
        bool flushRequested_{};
        bool closeRequested_{};
        bool closeSaveDecisionPending_{};
        bool closeSaveDecisionDialogOpen_{};
        bool closeWithoutSavingApproved_{};
        bool closingFromParent_{};
        bool closed_{};
        bool closeCallInProgress_{};
        bool settingsActionInFlight_{};
        bool settingsRefreshPending_{};
        bool paneStateKnown_{};
        bool paneStateRequestInFlight_{};
        std::optional<bool> pendingPaneState_;
        std::optional<bool> paneStateRequestValue_;
        bool appearanceColorDraft_{};
        std::optional<std::wstring> claudeColorDraft_;
        std::optional<std::wstring> codexColorDraft_;
        std::optional<std::wstring> antigravityColorDraft_;
        std::optional<std::wstring> opencodeColorDraft_;
        std::optional<std::wstring> cursorColorDraft_;
        std::optional<std::wstring> grokColorDraft_;
        std::optional<std::wstring> copilotColorDraft_;
        std::optional<std::wstring> usageTextColorDraft_;
        std::optional<std::wstring> claudeColorApplyText_;
        std::optional<std::wstring> codexColorApplyText_;
        std::optional<std::wstring> antigravityColorApplyText_;
        std::optional<std::wstring> opencodeColorApplyText_;
        std::optional<std::wstring> cursorColorApplyText_;
        std::optional<std::wstring> grokColorApplyText_;
        std::optional<std::wstring> copilotColorApplyText_;
        std::optional<std::wstring> usageTextColorApplyText_;
        std::optional<std::wstring> flightClaudeColorApplyText_;
        std::optional<std::wstring> flightCodexColorApplyText_;
        std::optional<std::wstring> flightAntigravityColorApplyText_;
        std::optional<std::wstring> flightOpencodeColorApplyText_;
        std::optional<std::wstring> flightCursorColorApplyText_;
        std::optional<std::wstring> flightGrokColorApplyText_;
        std::optional<std::wstring> flightCopilotColorApplyText_;
        std::optional<std::wstring> flightUsageTextColorApplyText_;
        std::optional<std::wstring> persistClaudeColorApplyText_;
        std::optional<std::wstring> persistCodexColorApplyText_;
        std::optional<std::wstring> persistAntigravityColorApplyText_;
        std::optional<std::wstring> persistOpencodeColorApplyText_;
        std::optional<std::wstring> persistCursorColorApplyText_;
        std::optional<std::wstring> persistGrokColorApplyText_;
        std::optional<std::wstring> persistCopilotColorApplyText_;
        std::optional<std::wstring> persistUsageTextColorApplyText_;
        uint64_t claudeColorDraftRevision_{};
        uint64_t codexColorDraftRevision_{};
        uint64_t antigravityColorDraftRevision_{};
        uint64_t opencodeColorDraftRevision_{};
        uint64_t cursorColorDraftRevision_{};
        uint64_t grokColorDraftRevision_{};
        uint64_t copilotColorDraftRevision_{};
        uint64_t usageTextColorDraftRevision_{};
        uint64_t resetColorsClaudeDraftRevision_{};
        uint64_t resetColorsCodexDraftRevision_{};
        uint64_t resetColorsAntigravityDraftRevision_{};
        uint64_t resetColorsOpencodeDraftRevision_{};
        uint64_t resetColorsCursorDraftRevision_{};
        uint64_t resetColorsGrokDraftRevision_{};
        uint64_t resetColorsCopilotDraftRevision_{};
        uint64_t resetColorsUsageTextDraftRevision_{};
        std::optional<std::wstring> deferredAction_;
        std::optional<std::wstring> settingsActionInFlightName_;
        bool syncingColorText_{};
        bool previewPending_{};
        bool previewInFlight_{};
        uint64_t widgetPreviewGeneration_{};
        uint64_t widgetPreviewFlightGeneration_{};
        uint64_t widgetPreviewImageGeneration_{};
        uint32_t widgetPreviewWidthPx_{};
        uint32_t widgetPreviewHeightPx_{};
        uint32_t widgetPreviewDpi_{ 96 };
        std::wstring latestWidgetPreviewBase64_;
        std::wstring displayedWidgetPreviewBase64_;
        std::vector<winrt::Microsoft::UI::Xaml::Controls::Image> appearancePresetImages_;
        std::vector<winrt::Microsoft::UI::Xaml::Controls::TextBlock> appearancePresetStatuses_;
        std::vector<std::pair<std::wstring, winrt::Microsoft::UI::Xaml::Controls::ToggleSwitch>> appearancePresetToggles_;
        std::vector<std::pair<std::wstring, winrt::Microsoft::UI::Xaml::Controls::ComboBox>> appearancePresetCombos_;
        std::vector<std::pair<std::wstring, winrt::Microsoft::UI::Xaml::Controls::TextBox>> appearancePresetColorInputs_;
        std::vector<AppearancePresetPreviewJob> appearancePresetPreviewJobs_;
        size_t appearancePresetPreviewNext_{};
        uint64_t appearancePresetGeneration_{};
        uint64_t appearancePresetFlightGeneration_{};
        uint32_t appearancePresetFlightIndex_{};
        bool presetPreviewInFlight_{};
        winrt::event_token closingToken_{};
        std::thread ioThread_;
        std::atomic<HANDLE> pipeHandle_{ INVALID_HANDLE_VALUE };
        HWND hwnd_{};
        HICON windowIcon_{};
        Microsoft::UI::Windowing::AppWindow appWindow_{ nullptr };
        Microsoft::UI::Xaml::Controls::NavigationView navigationView_{ nullptr };
        winrt::Microsoft::UI::Xaml::Window noticesWindow_{ nullptr };
        winrt::Microsoft::UI::Xaml::XamlRoot xamlRoot_{ nullptr };
        winrt::event_token windowClosedToken_{};
        winrt::event_token appWindowChangedToken_{};
        winrt::event_token xamlRootChangedToken_{};
        winrt::event_token navigationLoadedToken_{};
    };
}

namespace winrt::SettingsUI::factory_implementation
{
    struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow> {};
}
