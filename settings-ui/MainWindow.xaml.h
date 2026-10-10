#pragma once
#include "MainWindow.g.h"
#include <atomic>
#include <condition_variable>
#include <mutex>
#include <queue>
#include <string>
#include <thread>
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
        void SetValue(std::wstring const& key, winrt::Windows::Data::Json::IJsonValue const& value);
        void Action(std::wstring const& action);
        void ShowMessage(std::wstring const& text, bool error = false);
        void Hydrate();
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
        winrt::Microsoft::UI::Dispatching::DispatcherQueue dispatcher_{ nullptr };
        std::atomic<bool> stopping_{ false };
        std::atomic<bool> hydrating_{ false };
        std::mutex requestsMutex_;
        std::condition_variable requestsChanged_;
        std::queue<std::string> requests_;
        uint32_t nextId_{ 1 };
        uint32_t inFlight_{ 0 };
        std::thread ioThread_;
        std::atomic<HANDLE> pipeHandle_{ INVALID_HANDLE_VALUE };
        HWND hwnd_{};
    };
}

namespace winrt::SettingsUI::factory_implementation
{
    struct MainWindow : MainWindowT<MainWindow, implementation::MainWindow> {};
}
