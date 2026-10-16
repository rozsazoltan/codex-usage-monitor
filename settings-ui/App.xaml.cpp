#include "pch.h"
#include "App.xaml.h"
#include "MainWindow.xaml.h"
#include "App.xaml.g.hpp"
#include "NativeLocalization.h"
#include <shobjidl_core.h>
#include <cwchar>
#include <iterator>
#include <string>
#include <vector>

namespace
{
    std::wstring StartupLanguage()
    {
        const auto language = GetUserDefaultUILanguage();
        switch (PRIMARYLANGID(language))
        {
        case LANG_DUTCH: return L"nl";
        case LANG_SPANISH: return L"es";
        case LANG_FRENCH: return L"fr";
        case LANG_GERMAN: return L"de";
        case LANG_JAPANESE: return L"ja";
        case LANG_KOREAN: return L"ko";
        case LANG_CHINESE:
        {
            const auto sublanguage = SUBLANGID(language);
            return sublanguage == SUBLANG_CHINESE_TRADITIONAL ||
                sublanguage == SUBLANG_CHINESE_HONGKONG || sublanguage == SUBLANG_CHINESE_MACAU
                ? L"zh-TW" : L"zh-CN";
        }
        case LANG_RUSSIAN: return L"ru";
        case LANG_PORTUGUESE: return L"pt-BR";
        default: return L"en";
        }
    }

    void WriteStartupDiagnostic(wchar_t const* phase, HRESULT code, wchar_t const* message) noexcept
    {
        wchar_t line[512]{};
        const auto characterCount = swprintf_s(line, std::size(line),
            L"Quota Usage Settings startup failure | phase=%ls | HRESULT=0x%08lX | %ls\r\n",
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
                path += L"QuotaUsage-SettingsUI-startup.log";
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
        const auto language = StartupLanguage();
        auto message = std::wstring(L"The settings window could not be initialized.");
        try
        {
            window_ = make<MainWindow>();
            phase = L"Window activation";
            message = L"The settings window could not be activated.";
            window_.Activate();
        }
        catch (hresult_error const& error)
        {
            WriteStartupDiagnostic(phase, error.code().value, message.c_str());
            const auto title = NativeLocalization::Translate(L"Quota Usage settings could not open", language);
            const auto body = NativeLocalization::Translate(message, language);
            MessageBoxW(nullptr, body.c_str(), title.c_str(), MB_OK | MB_ICONERROR);
            Exit();
        }
        catch (...)
        {
            WriteStartupDiagnostic(phase, E_FAIL, L"Settings startup failed with an unexpected error.");
            const auto title = NativeLocalization::Translate(L"Quota Usage settings could not open", language);
            const auto body = NativeLocalization::Translate(message, language);
            MessageBoxW(nullptr, body.c_str(), title.c_str(), MB_OK | MB_ICONERROR);
            Exit();
        }
    }
}
