#include "pch.h"
#include "App.xaml.h"
#include "MainWindow.xaml.h"
#include "App.xaml.g.hpp"
#include <shobjidl_core.h>
#include <cwchar>
#include <iterator>
#include <string>
#include <vector>

namespace
{
    void WriteStartupDiagnostic(wchar_t const* phase, HRESULT code, wchar_t const* message) noexcept
    {
        wchar_t line[512]{};
        const auto characterCount = swprintf_s(line, std::size(line),
            L"Codex Usage Settings startup failure | phase=%ls | HRESULT=0x%08lX | %ls\r\n",
            phase, static_cast<unsigned long>(code), message);
        if (characterCount <= 0) return;

        OutputDebugStringW(line);
        char utf8[1024]{};
        const auto byteCount = WideCharToMultiByte(CP_UTF8, 0, line, characterCount, utf8,
            static_cast<int>(sizeof(utf8)), nullptr, nullptr);
        if (byteCount <= 0) return;

        try
        {
            std::vector<wchar_t> buffer(32768);
            auto openLogAt = [&](DWORD length)
            {
                std::wstring path(buffer.data(), length);
                if (path.back() != L'\\' && path.back() != L'/') path.push_back(L'\\');
                path += L"CodexUsage-SettingsUI-startup.log";
                return CreateFileW(path.c_str(), FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE,
                    nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
            };

            auto length = GetEnvironmentVariableW(L"LOCALAPPDATA", buffer.data(), static_cast<DWORD>(buffer.size()));
            HANDLE file = length > 0 && length < buffer.size() ? openLogAt(length) : INVALID_HANDLE_VALUE;
            if (file == INVALID_HANDLE_VALUE)
            {
                length = GetTempPathW(static_cast<DWORD>(buffer.size()), buffer.data());
                if (length == 0 || length >= buffer.size()) return;
                file = openLogAt(static_cast<DWORD>(wcslen(buffer.data())));
            }
            if (file == INVALID_HANDLE_VALUE) return;

            DWORD written{};
            WriteFile(file, utf8, static_cast<DWORD>(byteCount), &written, nullptr);
            FlushFileBuffers(file);
            CloseHandle(file);
        }
        catch (...) {}
    }
}

namespace winrt::SettingsUI::implementation
{
    App::App()
    {
        SetCurrentProcessExplicitAppUserModelID(L"com.datarose.codexusage");
        try
        {
            InitializeComponent();
        }
        catch (hresult_error const& error)
        {
            WriteStartupDiagnostic(L"App.InitializeComponent", error.code().value,
                L"Application XAML initialization failed.");
            throw;
        }
        catch (...)
        {
            WriteStartupDiagnostic(L"App.InitializeComponent", E_FAIL,
                L"Application XAML initialization failed with an unexpected error.");
            throw;
        }
    }

    void App::OnLaunched(Microsoft::UI::Xaml::LaunchActivatedEventArgs const&)
    {
        auto phase = L"MainWindow construction";
        auto message = L"The settings window could not be initialized.";
        try
        {
            window_ = make<MainWindow>();
            phase = L"Window activation";
            message = L"The settings window could not be activated.";
            window_.Activate();
        }
        catch (hresult_error const& error)
        {
            WriteStartupDiagnostic(phase, error.code().value, message);
            MessageBoxW(nullptr, error.message().c_str(), L"Codex Usage settings could not open", MB_OK | MB_ICONERROR);
            Exit();
        }
        catch (...)
        {
            WriteStartupDiagnostic(phase, E_FAIL, L"Settings startup failed with an unexpected error.");
            MessageBoxW(nullptr, message, L"Codex Usage settings could not open", MB_OK | MB_ICONERROR);
            Exit();
        }
    }
}
