#include "pch.h"
#include "App.xaml.h"
#include "MainWindow.xaml.h"
#include "App.xaml.g.hpp"

namespace winrt::SettingsUI::implementation
{
    App::App()
    {
        InitializeComponent();
    }

    void App::OnLaunched(Microsoft::UI::Xaml::LaunchActivatedEventArgs const&)
    {
        try
        {
            window_ = make<MainWindow>();
            window_.Activate();
        }
        catch (hresult_error const& error)
        {
            MessageBoxW(nullptr, error.message().c_str(), L"Codex Usage settings could not open", MB_OK | MB_ICONERROR);
            Exit();
        }
    }
}
