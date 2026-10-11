#pragma once
#include "MainWindow.g.h"
#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <mutex>
#include <optional>
#include <queue>
#include <string>
#include <thread>
#include <winrt/Microsoft.UI.Dispatching.h>
#include <winrt/Microsoft.UI.Xaml.Controls.h>
#include <winrt/Windows.Data.Json.h>

namespace winrt::SettingsUI::implementation
{
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
        void ContinueDeferredWork();
        bool HasPendingChanges() const;
        void OnWindowClosing(Microsoft::UI::Windowing::AppWindow const&,
            Microsoft::UI::Windowing::AppWindowClosingEventArgs const& args);
        void SetValue(std::wstring const& key, winrt::Windows::Data::Json::IJsonValue const& value);
        void Action(std::wstring const& action);
        void ShowMessage(std::wstring const& text, bool error = false);
        void UpdateClaudeAvailability();
        void UpdateAboutControls();
        void UpdateTitleBarLayout();
        void OpenThirdPartyNotices();
        void Hydrate();
        void RefreshSettingsControlsIfSafe();
        void BuildGeneral();
        void BuildMonitoring();
        void BuildAppearance();
        void BuildPlacement();
        void BuildAbout();
        winrt::Windows::Data::Json::JsonObject Setting(std::wstring const& key);
        bool Bool(std::wstring const& key);
        std::wstring Text(std::wstring const& key);
        winrt::Windows::Data::Json::JsonObject State() const;

    private:
        std::wstring pipeName_;
        uint32_t parentPid_{};
        std::wstring page_{ L"General" };
        winrt::Windows::Data::Json::JsonObject state_{ nullptr };
        winrt::Windows::Data::Json::JsonObject flightChanges_{ nullptr };
        winrt::Windows::Data::Json::JsonObject pendingChanges_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::ToggleSwitch claudeToggle_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock versionText_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock updateStatusText_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::TextBlock uninstallDescription_{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::Button checkForUpdatesButton_{ nullptr };
        winrt::Microsoft::UI::Dispatching::DispatcherQueue dispatcher_{ nullptr };
        winrt::Microsoft::UI::Dispatching::DispatcherQueueTimer debounceTimer_{ nullptr };
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
        bool mutationInFlight_{};
        bool persistInFlight_{};
        bool persistNeeded_{};
        bool flushRequested_{};
        bool closeRequested_{};
        bool closingFromParent_{};
        bool debounceElapsed_{};
        bool settingsActionInFlight_{};
        bool settingsRefreshPending_{};
        bool paneStateKnown_{};
        bool appearanceColorDraft_{};
        bool claudeColorDraft_{};
        bool codexColorDraft_{};
        std::optional<std::wstring> deferredAction_;
        winrt::event_token closingToken_{};
        std::thread ioThread_;
        std::atomic<HANDLE> pipeHandle_{ INVALID_HANDLE_VALUE };
        HWND hwnd_{};
        HICON windowIcon_{};
        winrt::Microsoft::UI::Xaml::Window noticesWindow_{ nullptr };
        winrt::Microsoft::UI::Xaml::XamlRoot xamlRoot_{ nullptr };
        winrt::event_token appWindowChangedToken_{};
        winrt::event_token xamlRootChangedToken_{};
        winrt::event_token navigationLoadedToken_{};
    };
}

namespace winrt::SettingsUI::factory_implementation
{
    struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow> {};
}
