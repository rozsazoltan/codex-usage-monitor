#include "pch.h"
#include "MainWindow.xaml.h"
#include "MainWindow.g.cpp"
#include "MainWindow.xaml.g.hpp"
#include "Resources.h"
#include "NativeLocalization.h"
#include <winrt/Microsoft.UI.Input.h>
#include <shellapi.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <iomanip>
#include <limits>
#include <map>
#include <sstream>
#include <vector>

using namespace winrt;
using namespace Microsoft::UI::Xaml;
using namespace Microsoft::UI::Xaml::Controls;
using namespace Windows::Data::Json;

namespace
{
    std::wstring g_nativeLanguage{ L"en" };
    std::string Utf8(std::wstring const& value)
    {
        if (value.empty()) return {};
        const auto size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
        std::string result(static_cast<size_t>(size), '\0');
        WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()), result.data(), size, nullptr, nullptr);
        return result;
    }
    std::wstring Wide(std::string const& value)
    {
        if (value.empty()) return {};
        const auto size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()), nullptr, 0);
        std::wstring result(static_cast<size_t>(size), L'\0');
        MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()), result.data(), size);
        return result;
    }
    JsonObject Request(std::wstring const& op)
    {
        auto request = JsonObject(); request.Insert(L"v", JsonValue::CreateNumberValue(1));
        request.Insert(L"op", JsonValue::CreateStringValue(op)); return request;
    }
    TextBlock Label(std::wstring const& text, double size = 14)
    {
        auto label = TextBlock(); label.Text(NativeLocalization::Translate(text, g_nativeLanguage)); label.FontSize(size); label.TextWrapping(TextWrapping::Wrap); return label;
    }
    HICON LoadResourceIcon()
    {
        return static_cast<HICON>(LoadImageW(GetModuleHandleW(nullptr), MAKEINTRESOURCEW(IDI_SETTINGS_APP), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE));
    }
    void ApplyWindowIcon(Microsoft::UI::Xaml::Window const& window, HICON icon)
    {
        if (!icon) return;
        const auto iconId = Microsoft::UI::GetIconIdFromIcon(icon);
        window.AppWindow().SetIcon(iconId);
        HWND hwnd{};
        if (SUCCEEDED(window.as<::IWindowNative>()->get_WindowHandle(&hwnd)))
        {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, reinterpret_cast<LPARAM>(icon));
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, reinterpret_cast<LPARAM>(icon));
        }
    }
    std::vector<uint8_t> ResourceBytes(int id)
    {
        auto module = GetModuleHandleW(nullptr);
        auto resource = FindResourceW(module, MAKEINTRESOURCEW(id), RT_RCDATA);
        if (!resource) return {};
        auto loaded = LoadResource(module, resource);
        const auto size = SizeofResource(module, resource);
        auto data = static_cast<uint8_t const*>(LockResource(loaded));
        return data && size ? std::vector<uint8_t>(data, data + size) : std::vector<uint8_t>{};
    }
    std::wstring ResourceText(int id)
    {
        const auto bytes = ResourceBytes(id);
        if (bytes.empty()) return L"Third-party notices could not be loaded.";
        const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, reinterpret_cast<char const*>(bytes.data()), static_cast<int>(bytes.size()), nullptr, 0);
        if (!length) return L"Third-party notices could not be loaded.";
        std::wstring text(static_cast<size_t>(length), L'\0');
        MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, reinterpret_cast<char const*>(bytes.data()), static_cast<int>(bytes.size()), text.data(), length);
        return text;
    }
    winrt::fire_and_forget SetImageFromResource(Image image, int id)
    {
        try
        {
            const auto bytes = ResourceBytes(id);
            if (bytes.empty()) co_return;
            Windows::Storage::Streams::InMemoryRandomAccessStream stream;
            Windows::Storage::Streams::DataWriter writer(stream);
            writer.WriteBytes(winrt::array_view<uint8_t const>(bytes.data(), bytes.data() + bytes.size()));
            co_await writer.StoreAsync();
            writer.DetachStream();
            stream.Seek(0);
            Media::Imaging::BitmapImage bitmap;
            co_await bitmap.SetSourceAsync(stream);
            image.Source(bitmap);
        }
        catch (...) {}
    }
    Border Card()
    {
        auto border = Border(); border.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
        border.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
        border.HorizontalAlignment(HorizontalAlignment::Stretch);
        border.BorderThickness(ThicknessHelper::FromUniformLength(1)); border.CornerRadius(CornerRadiusHelper::FromUniformRadius(12)); border.Padding(Thickness{24,24,24,24}); return border;
    }
    Grid ToggleRow(StackPanel const& copy, ToggleSwitch const& toggle)
    {
        auto row = Grid();
        auto columns = row.ColumnDefinitions();
        columns.Append(ColumnDefinition());
        columns.Append(ColumnDefinition());
        // Let the native ToggleSwitch choose its full desired width. A fixed
        // column can be narrower than the control template and clip its label.
        columns.GetAt(1).Width(GridLengthHelper::Auto());
        row.HorizontalAlignment(HorizontalAlignment::Stretch);
        copy.VerticalAlignment(VerticalAlignment::Center);
        copy.HorizontalAlignment(HorizontalAlignment::Stretch);
        toggle.HorizontalAlignment(HorizontalAlignment::Right);
        toggle.VerticalAlignment(VerticalAlignment::Center);
        toggle.Margin(Thickness{12, 0, 0, 0});
        row.Children().Append(copy);
        Grid::SetColumn(toggle, 1);
        row.Children().Append(toggle);
        return row;
    }
    void SetSwitchContent(ToggleSwitch const& toggle)
    {
        toggle.OnContent(box_value(NativeLocalization::Translate(L"On", g_nativeLanguage)));
        toggle.OffContent(box_value(NativeLocalization::Translate(L"Off", g_nativeLanguage)));
    }
    bool SettingsChanged(JsonObject const& previous, JsonObject const& updated)
    {
        const auto before = previous ? previous.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        const auto after = updated ? updated.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        if (before.Size() != after.Size()) return true;
        for (auto const& pair : before)
            if (!after.HasKey(pair.Key()) || after.GetNamedValue(pair.Key()).Stringify() != pair.Value().Stringify()) return true;
        return false;
    }
    bool IsSettingsChangingAction(std::optional<std::wstring> const& action)
    {
        return action && (*action == L"recommended" || *action == L"reset_appearance" || *action == L"reset_colors");
    }
    std::optional<std::array<uint8_t, 3>> ParseHexColor(std::wstring const& text)
    {
        if (text.size() != 7 || text[0] != L'#') return std::nullopt;
        std::array<uint8_t, 3> color{};
        auto hexDigit = [](wchar_t value) -> int
        {
            if (value >= L'0' && value <= L'9') return value - L'0';
            if (value >= L'A' && value <= L'F') return value - L'A' + 10;
            if (value >= L'a' && value <= L'f') return value - L'a' + 10;
            return -1;
        };
        for (size_t i = 0; i < color.size(); ++i)
        {
            const auto high = hexDigit(text[1 + i * 2]);
            const auto low = hexDigit(text[2 + i * 2]);
            if (high < 0 || low < 0) return std::nullopt;
            color[i] = static_cast<uint8_t>((high << 4) | low);
        }
        return color;
    }
    std::wstring FormatHexColor(std::array<uint8_t, 3> const& color)
    {
        constexpr wchar_t digits[] = L"0123456789ABCDEF";
        std::wstring text{ L'#' };
        for (const auto value : color)
        {
            text.push_back(digits[value >> 4]);
            text.push_back(digits[value & 0x0F]);
        }
        return text;
    }
    std::wstring StoredColorText(JsonObject const& settings, std::wstring const& key)
    {
        const auto value = settings.GetNamedValue(key, JsonValue::CreateNullValue());
        if (value.ValueType() != JsonValueType::Array) return {};
        const auto array = value.GetArray();
        if (array.Size() != 3) return {};
        std::array<uint8_t, 3> color{};
        for (uint32_t i = 0; i < color.size(); ++i)
        {
            const auto component = array.GetAt(i).GetNumber();
            if (component < 0 || component > 255 || component != static_cast<int>(component)) return {};
            color[i] = static_cast<uint8_t>(component);
        }
        return FormatHexColor(color);
    }
    std::wstring EffectiveColorText(JsonObject const& state, std::wstring const& key)
    {
        if (!state || !state.HasKey(L"effective_appearance")) return {};
        const auto effective = state.GetNamedObject(L"effective_appearance", JsonObject());
        if (!effective.HasKey(key) || effective.GetNamedValue(key).ValueType() != JsonValueType::Array) return {};
        const auto array = effective.GetNamedArray(key);
        if (array.Size() != 3) return {};
        std::array<uint8_t, 3> color{};
        for (uint32_t i = 0; i < color.size(); ++i)
        {
            const auto component = array.GetAt(i).GetNumber();
            if (component < 0 || component > 255 || component != static_cast<int>(component)) return {};
            color[i] = static_cast<uint8_t>(component);
        }
        return FormatHexColor(color);
    }
    bool SettingsChangesAreLocalEcho(JsonObject const& previous, JsonObject const& updated, JsonObject const& localChanges)
    {
        const auto before = previous ? previous.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        const auto after = updated ? updated.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        bool changed = false;
        for (auto const& pair : before)
        {
            if (!after.HasKey(pair.Key())) return false;
            if (after.GetNamedValue(pair.Key()).Stringify() == pair.Value().Stringify()) continue;
            changed = true;
            if (!localChanges || !localChanges.HasKey(pair.Key()) ||
                localChanges.GetNamedValue(pair.Key()).Stringify() != after.GetNamedValue(pair.Key()).Stringify()) return false;
        }
        for (auto const& pair : after)
        {
            if (before.HasKey(pair.Key())) continue;
            changed = true;
            if (!localChanges || !localChanges.HasKey(pair.Key()) ||
                localChanges.GetNamedValue(pair.Key()).Stringify() != pair.Value().Stringify()) return false;
        }
        return changed;
    }
}

namespace winrt::SettingsUI::implementation
{
    MainWindow::MainWindow()
    {
        InitializeComponent();
        dispatcher_ = Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread();
        appWindow_ = AppWindow();
        navigationView_ = Navigation();
        updateIndicator_ = InfoBadge();
        updateIndicator_.Background(Media::SolidColorBrush(Windows::UI::Color{ 255, 0, 120, 212 }));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(updateIndicator_, NativeLocalization::Translate(L"Update available", g_nativeLanguage));
        NavigationUpdate().InfoBadge(updateIndicator_);
        NavigationUpdate().Visibility(Visibility::Collapsed);
        auto weak = get_weak();
#if QUOTA_DEVELOPMENT_UI
        auto developmentShortcut = Input::KeyboardAccelerator();
        developmentShortcut.Key(Windows::System::VirtualKey::F12);
        developmentShortcut.Invoked([weak](auto const&, auto const& args)
        {
            if (auto self = weak.get(); self && !self->closed_ && self->state_ &&
                self->state_.GetNamedBoolean(L"development_enabled", false))
            {
                self->OpenDevelopment();
                args.Handled(true);
            }
        });
        Navigation().KeyboardAccelerators().Append(developmentShortcut);
#endif
        previewTimer_ = dispatcher_.CreateTimer();
        previewTimer_.Interval(std::chrono::milliseconds(180));
        previewTimer_.IsRepeating(false);
        previewTimer_.Tick([weak](auto const&, auto const&)
        {
            if (auto self = weak.get(); self && !self->closed_)
            {
                self->previewPending_ = true;
                self->SendWidgetPreviewIfIdle();
            }
        });
        messageTimer_ = dispatcher_.CreateTimer();
        messageTimer_.Interval(std::chrono::seconds(10));
        messageTimer_.IsRepeating(false);
        messageTimer_.Tick([weak](auto const&, auto const&)
        {
            if (auto self = weak.get(); self && !self->closed_) self->MessageBar().IsOpen(false);
        });
        Navigation().PaneOpened([this](auto const&, auto const&)
        {
            UpdateUpdateIndicator();
            if (paneStateKnown_ && !hydrating_) QueuePaneState(true);
        });
        Navigation().PaneClosed([this](auto const&, auto const&)
        {
            UpdateUpdateIndicator();
            if (paneStateKnown_ && !hydrating_) QueuePaneState(false);
        });
        SaveChangesButton().Click([this](auto const&, auto const&) { FlushPending(); });
        DiscardChangesButton().Click([this](auto const&, auto const&) { DiscardPendingChanges(); });
        pageScroll_ = PageScroll();
        pageScrollViewToken_ = pageScroll_.ViewChanged([weak](auto const&, auto const&)
        {
            if (auto self = weak.get(); self && !self->closed_) self->UpdateStickyPreview();
        });
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd_));
        windowIcon_ = LoadResourceIcon();
        ApplyWindowIcon(*this, windowIcon_);
        windowClosedToken_ = Closed([weak](auto const&, auto const&)
        {
            if (auto self = weak.get()) self->OnWindowClosed();
        });
        ExtendsContentIntoTitleBar(true);
        SetTitleBar(TitleBarDragRegion());
        appWindow_.TitleBar().IconShowOptions(Microsoft::UI::Windowing::IconShowOptions::HideIconAndSystemMenu);
        UpdateTitleBarLayout();
        appWindowChangedToken_ = appWindow_.Changed([this](auto const&, auto const& args)
        {
            if (args.DidPositionChange() || args.DidSizeChange()) UpdateTitleBarLayout();
        });
        navigationLoadedToken_ = navigationView_.Loaded([this](auto const& sender, auto const&)
        {
            xamlRoot_ = sender.as<NavigationView>().XamlRoot();
            if (xamlRoot_)
                xamlRootChangedToken_ = xamlRoot_.Changed([this](auto const&, auto const&) { UpdateTitleBarLayout(); });
            UpdateTitleBarLayout();
        });
        closingToken_ = appWindow_.Closing([this](auto const& sender, auto const& args) { OnWindowClosing(sender, args); });
        MONITORINFO monitor{ sizeof(monitor) };
        if (GetMonitorInfoW(MonitorFromWindow(hwnd_, MONITOR_DEFAULTTONEAREST), &monitor))
        {
            const auto dpi = GetDpiForWindow(hwnd_);
            const auto width = std::min<int>(MulDiv(1080, dpi, 96), monitor.rcWork.right - monitor.rcWork.left);
            const auto height = std::min<int>(MulDiv(760, dpi, 96), monitor.rcWork.bottom - monitor.rcWork.top);
            appWindow_.MoveAndResize({ monitor.rcWork.left + (monitor.rcWork.right - monitor.rcWork.left - width) / 2,
                monitor.rcWork.top + (monitor.rcWork.bottom - monitor.rcWork.top - height) / 2, width, height });
        }
        HIGHCONTRASTW contrast{ sizeof(contrast) };
        const bool highContrast = SystemParametersInfoW(SPI_GETHIGHCONTRAST, sizeof(contrast), &contrast, 0) && (contrast.dwFlags & HCF_HIGHCONTRASTON);
        OSVERSIONINFOW iconVersion{ sizeof(iconVersion) };
        using RtlGetVersionFn = LONG(WINAPI*)(OSVERSIONINFOW*);
        auto ntdll = GetModuleHandleW(L"ntdll.dll");
        auto getVersion = reinterpret_cast<RtlGetVersionFn>(GetProcAddress(ntdll, "RtlGetVersion"));
        if (getVersion && getVersion(&iconVersion) == 0 && iconVersion.dwBuildNumber >= 22000)
        {
            auto chemistryIcon = FontIcon();
            chemistryIcon.Glyph(L"\xF196");
            chemistryIcon.FontFamily(Microsoft::UI::Xaml::Media::FontFamily(L"Segoe Fluent Icons"));
            NavigationExperimental().Icon(chemistryIcon);
        }
        if (highContrast)
        {
            SystemBackdrop(nullptr);
            auto color = GetSysColor(COLOR_WINDOW);
            Navigation().Background(Media::SolidColorBrush(Windows::UI::Color{ 255, GetRValue(color), GetGValue(color), GetBValue(color) }));
        }
        else
        {
            OSVERSIONINFOW version{ sizeof(version) };
            if (getVersion && getVersion(&version) == 0 && version.dwBuildNumber < 22000)
            {
                SystemBackdrop(nullptr);
                Navigation().Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
            }
        }
        int count = 0;
        auto args = CommandLineToArgvW(GetCommandLineW(), &count);
        if (args)
        {
            for (int i = 1; args && i + 1 < count; ++i)
            {
                if (std::wstring(args[i]) == L"--pipe") pipeName_ = args[++i];
                else if (std::wstring(args[i]) == L"--page") { page_ = args[++i]; if (!page_.empty()) page_[0] = static_cast<wchar_t>(towupper(page_[0])); }
                else if (std::wstring(args[i]) == L"--parent-pid") parentPid_ = static_cast<uint32_t>(_wtoi(args[++i]));
            }
            if (args) LocalFree(args);
        }
        const auto requestedPage = page_;
        page_ = L"";
        Navigation().SelectedItem(Navigation().MenuItems().GetAt(0));
#if QUOTA_DEVELOPMENT_UI
        if (requestedPage == L"Development") OpenDevelopment();
#endif
        for (auto const& item : Navigation().MenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == requestedPage) Navigation().SelectedItem(item);
        for (auto const& item : Navigation().FooterMenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == requestedPage) Navigation().SelectedItem(item);
        if (pipeName_.empty()) ShowMessage(L"Settings could not connect to the widget. Open this window from the Quota Usage widget.", true);
        else InitializePipe();
    }

    MainWindow::~MainWindow()
    {
        RevokeWindowEvents();
        if (closeSaveFailureDialog_)
        {
            closeSaveFailureDialog_.Hide();
            closeSaveFailureDialog_ = nullptr;
        }
        if (noticesWindow_)
        {
            noticesWindow_.Close();
            noticesWindow_ = nullptr;
        }
        if (messageTimer_) messageTimer_.Stop();
        if (previewTimer_) previewTimer_.Stop();
        stopping_.store(true); requestsChanged_.notify_all();
        if (pipeHandle_ != INVALID_HANDLE_VALUE) CancelIoEx(pipeHandle_, nullptr);
        if (ioThread_.joinable()) CancelSynchronousIo(ioThread_.native_handle());
        if (ioThread_.joinable()) ioThread_.join();
        if (windowIcon_) DestroyIcon(windowIcon_);
    }

    void MainWindow::RevokeWindowEvents()
    {
        if (appWindow_ && appWindowChangedToken_) appWindow_.Changed(appWindowChangedToken_);
        appWindowChangedToken_ = {};
        if (appWindow_ && closingToken_) appWindow_.Closing(closingToken_);
        closingToken_ = {};
        if (navigationView_ && navigationLoadedToken_) navigationView_.Loaded(navigationLoadedToken_);
        navigationLoadedToken_ = {};
        if (pageScroll_ && pageScrollViewToken_) pageScroll_.ViewChanged(pageScrollViewToken_);
        pageScrollViewToken_ = {};
        if (xamlRoot_ && xamlRootChangedToken_) xamlRoot_.Changed(xamlRootChangedToken_);
        xamlRootChangedToken_ = {};
        xamlRoot_ = nullptr;
        if (windowClosedToken_) Closed(windowClosedToken_);
        windowClosedToken_ = {};
        appWindow_ = nullptr;
        navigationView_ = nullptr;
    }

    void MainWindow::OnWindowClosed()
    {
        if (closed_) return;
        CancelProviderReorder();
        closed_ = true;

        stopping_.store(true);
        requestsChanged_.notify_all();
        const auto pipe = pipeHandle_.load();
        if (pipe != INVALID_HANDLE_VALUE) CancelIoEx(pipe, nullptr);
        if (messageTimer_) messageTimer_.Stop();
        if (previewTimer_) previewTimer_.Stop();
        RevokeWindowEvents();

        if (closeSaveFailureDialog_)
        {
            closeSaveFailureDialog_.Hide();
            closeSaveFailureDialog_ = nullptr;
            closeSaveDecisionDialogOpen_ = false;
        }
        if (noticesWindow_)
        {
            auto window = noticesWindow_;
            noticesWindow_ = nullptr;
            window.Close();
        }
    }

    void MainWindow::CloseIfOpen()
    {
        if (closed_ || closeCallInProgress_) return;
        closeCallInProgress_ = true;
        struct CloseCallReset
        {
            bool& inProgress;
            ~CloseCallReset() { inProgress = false; }
        } reset{ closeCallInProgress_ };
        Close();
    }

    void MainWindow::InitializePipe()
    {
        auto get = Request(L"get"); inFlight_ = nextId_++; initialGetPending_ = true;
        get.Insert(L"id", JsonValue::CreateNumberValue(inFlight_));
        const auto initialRequest = Utf8(get.Stringify().c_str());
        auto weak = get_weak();
        auto dispatcher = dispatcher_;
        ioThread_ = std::thread([this, initialRequest, weak, dispatcher]()
        {
            try { winrt::init_apartment(winrt::apartment_type::multi_threaded); }
            catch (...)
            {
                stopping_ = true;
                dispatcher.TryEnqueue([weak]()
                {
                    if (auto self = weak.get(); self && !self->closed_)
                        self->ShowMessage(L"The settings connection could not initialize.", true);
                });
                return;
            }
            struct ApartmentCleanup { ~ApartmentCleanup() { winrt::uninit_apartment(); } } apartmentCleanup;
            HANDLE pipe = INVALID_HANDLE_VALUE;
            for (int attempt = 0; attempt < 150 && !stopping_; ++attempt)
            {
                pipe = CreateFileW(pipeName_.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, nullptr);
                if (pipe != INVALID_HANDLE_VALUE) break;
                if (GetLastError() != ERROR_PIPE_BUSY && GetLastError() != ERROR_FILE_NOT_FOUND) break;
                WaitNamedPipeW(pipeName_.c_str(), 100);
            }
            if (pipe == INVALID_HANDLE_VALUE)
            {
                dispatcher.TryEnqueue([weak]()
                {
                    if (auto self = weak.get(); self && !self->closed_)
                        self->ShowMessage(L"Could not connect to Quota Usage. Close this window and try again from the widget.", true);
                });
                return;
            }
            ULONG serverPid = 0;
            if (!GetNamedPipeServerProcessId(pipe, &serverPid) || !parentPid_ || serverPid != parentPid_)
            {
                CloseHandle(pipe);
                dispatcher.TryEnqueue([weak]()
                {
                    if (auto self = weak.get(); self && !self->closed_)
                        self->ShowMessage(L"The settings connection could not be verified. Reopen settings from the widget.", true);
                });
                stopping_ = true; return;
            }
            pipeHandle_ = pipe;
            auto writeAll = [pipe](void const* data, DWORD size)
            {
                auto bytes = static_cast<uint8_t const*>(data); DWORD total = 0;
                while (total < size)
                {
                    OVERLAPPED overlapped{}; overlapped.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
                    if (!overlapped.hEvent) return false;
                    DWORD count = 0;
                    BOOL ok = WriteFile(pipe, bytes + total, size - total, &count, &overlapped);
                    if (!ok && GetLastError() == ERROR_IO_PENDING)
                        ok = GetOverlappedResult(pipe, &overlapped, &count, TRUE);
                    CloseHandle(overlapped.hEvent);
                    if (!ok || !count) return false;
                    total += count;
                }
                return true;
            };
            const auto initialLength = static_cast<uint32_t>(initialRequest.size());
            std::array<uint8_t, 4> initialPrefix{ static_cast<uint8_t>(initialLength), static_cast<uint8_t>(initialLength >> 8), static_cast<uint8_t>(initialLength >> 16), static_cast<uint8_t>(initialLength >> 24) };
            if (!writeAll(initialPrefix.data(), 4) || !writeAll(initialRequest.data(), initialLength))
            {
                pipeHandle_ = INVALID_HANDLE_VALUE;
                CloseHandle(pipe);
                dispatcher.TryEnqueue([weak]()
                {
                    if (auto self = weak.get(); self && !self->closed_)
                        self->ShowMessage(L"The first settings request could not be sent. Reopen settings from the widget.", true);
                });
                stopping_ = true; return;
            }
            auto readExact = [pipe](void* data, DWORD size)
            {
                auto bytes = static_cast<uint8_t*>(data); DWORD total = 0;
                while (total < size)
                {
                    OVERLAPPED overlapped{}; overlapped.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
                    if (!overlapped.hEvent) return false;
                    DWORD count = 0;
                    BOOL ok = ReadFile(pipe, bytes + total, size - total, &count, &overlapped);
                    if (!ok && GetLastError() == ERROR_IO_PENDING)
                        ok = GetOverlappedResult(pipe, &overlapped, &count, TRUE);
                    CloseHandle(overlapped.hEvent);
                    if (!ok || !count) return false;
                    total += count;
                }
                return true;
            };
            auto writer = std::thread([this, pipe, writeAll]()
            {
                while (!stopping_)
                {
                    std::string body;
                    {
                        std::unique_lock lock(requestsMutex_);
                        requestsChanged_.wait(lock, [this]() { return stopping_ || !requests_.empty(); });
                        if (stopping_) break;
                        body = std::move(requests_.front()); requests_.pop();
                    }
                    const auto length = static_cast<uint32_t>(body.size());
                    std::array<uint8_t, 4> prefix{ static_cast<uint8_t>(length), static_cast<uint8_t>(length >> 8), static_cast<uint8_t>(length >> 16), static_cast<uint8_t>(length >> 24) };
                    if (!writeAll(prefix.data(), 4) || !writeAll(body.data(), length))
                    {
                        stopping_ = true; CancelIoEx(pipe, nullptr); requestsChanged_.notify_all(); break;
                    }
                }
            });
            bool connected = true;
            while (!stopping_ && connected)
            {
                uint8_t prefix[4]{};
                if (!readExact(prefix, 4)) break;
                const auto length = uint32_t(prefix[0]) | (uint32_t(prefix[1]) << 8) | (uint32_t(prefix[2]) << 16) | (uint32_t(prefix[3]) << 24);
                if (length == 0 || length > 2 * 1024 * 1024) break;
                std::string body(length, '\0');
                if (!readExact(body.data(), length)) break;
                try
                {
                    auto message = JsonObject::Parse(Wide(body));
                    dispatcher.TryEnqueue([weak, message]()
                    {
                        if (auto self = weak.get(); self && !self->closed_) self->Receive(message);
                    });
                }
                catch (...) { connected = false; }
            }
            stopping_.store(true); requestsChanged_.notify_all(); CancelIoEx(pipe, nullptr); writer.join();
            pipeHandle_ = INVALID_HANDLE_VALUE;
            CloseHandle(pipe);
            dispatcher.TryEnqueue([weak]()
            {
                if (auto self = weak.get(); self && !self->closed_) self->CloseIfOpen();
            });
        });
    }

    void MainWindow::Send(JsonObject request)
    {
        if (stopping_) return;
        if (inFlight_ != 0) return;
        inFlight_ = nextId_++;
        request.Insert(L"id", JsonValue::CreateNumberValue(inFlight_));
        const auto text = Utf8(request.Stringify().c_str());
        if (text.empty() || text.size() > 65536) { inFlight_ = 0; ShowMessage(L"The request could not be sent.", true); return; }
        { std::lock_guard lock(requestsMutex_); requests_.push(text); }
        requestsChanged_.notify_one();
    }

    bool MainWindow::HasPendingChanges() const { return pendingChanges_ && pendingChanges_.Size() != 0; }

    void MainWindow::FlushPending()
    {
        flushRequested_ = true;
        SendNextMutation();
    }

    void MainWindow::UpdateSaveBar()
    {
        const bool saveInProgress = mutationInFlight_ || persistInFlight_;
        const bool dirty = !closed_ && (HasPendingChanges() || HasUnappliedColorDraft() ||
            (persistNeeded_ && !saveInProgress));
        SaveBar().Visibility(dirty ? Visibility::Visible : Visibility::Collapsed);
        UpdateToastPosition();
    }

    void MainWindow::UpdateToastPosition()
    {
        if (!MessageBar() || !SaveBar()) return;
        const bool saveVisible = SaveBar().Visibility() == Visibility::Visible;
        MessageBar().Margin(Thickness{ 24.0, 0.0, 24.0, saveVisible ? 76.0 : 18.0 });
    }

    void MainWindow::DiscardPendingChanges()
    {
        if (mutationInFlight_ || persistInFlight_)
        {
            ShowMessage(L"A save is already in progress. Please wait a moment and try again.", true);
            return;
        }
        pendingChanges_ = nullptr;
        flushRequested_ = false;
        persistNeeded_ = false;
        SetColorDraft(L"claude_accent", std::nullopt);
        SetColorDraft(L"codex_accent", std::nullopt);
        SetColorDraft(L"antigravity_accent", std::nullopt);
        SetColorDraft(L"opencode_accent", std::nullopt);
        SetColorDraft(L"cursor_accent", std::nullopt);
        SetColorDraft(L"grok_accent", std::nullopt);
        SetColorDraft(L"copilot_accent", std::nullopt);
        SetColorDraft(L"usage_text_color", std::nullopt);
        claudeColorApplyText_.reset();
        codexColorApplyText_.reset();
        antigravityColorApplyText_.reset();
        opencodeColorApplyText_.reset();
        cursorColorApplyText_.reset();
        grokColorApplyText_.reset();
        copilotColorApplyText_.reset();
        usageTextColorApplyText_.reset();
        Hydrate();
        UpdateSaveBar();
        ScheduleWidgetPreview();
        ShowMessage(L"Unsaved changes were discarded.");
    }

    void MainWindow::SendNextMutation()
    {
        if (stopping_ || inFlight_ != 0) return;
        if (HasPendingChanges())
        {
            auto server = state_ ? state_.GetNamedObject(L"settings", JsonObject()) : JsonObject();
            auto changes = JsonArray();
            auto snapshot = JsonObject();
            auto remainder = JsonObject();
            for (auto const& pair : pendingChanges_)
            {
                const auto key = std::wstring(pair.Key().c_str());
                const bool colorApplyPending = key == L"claude_accent" ? claudeColorApplyText_.has_value()
                    : key == L"codex_accent" ? codexColorApplyText_.has_value()
                    : key == L"antigravity_accent" ? antigravityColorApplyText_.has_value()
                    : key == L"opencode_accent" ? opencodeColorApplyText_.has_value()
                    : key == L"cursor_accent" ? cursorColorApplyText_.has_value()
                    : key == L"grok_accent" ? grokColorApplyText_.has_value()
                    : key == L"copilot_accent" ? copilotColorApplyText_.has_value()
                    : key == L"usage_text_color" ? usageTextColorApplyText_.has_value() : false;
                if ((!flightChanges_ || !flightChanges_.HasKey(key)) && !colorApplyPending &&
                    server.HasKey(key) && server.GetNamedValue(key).Stringify() == pair.Value().Stringify()) continue;
                if (changes.Size() == 20) { remainder.Insert(key, pair.Value()); continue; }
                auto change = JsonObject(); change.Insert(L"key", JsonValue::CreateStringValue(key)); change.Insert(L"value", pair.Value()); changes.Append(change);
                snapshot.Insert(key, pair.Value());
            }
            pendingChanges_ = remainder;
            UpdateSaveBar();
            if (changes.Size() != 0)
            {
                auto request = Request(L"set_batch"); request.Insert(L"changes", changes);
                flightChanges_ = snapshot;
                flightClaudeColorApplyText_ = snapshot.HasKey(L"claude_accent") ? claudeColorApplyText_ : std::nullopt;
                flightCodexColorApplyText_ = snapshot.HasKey(L"codex_accent") ? codexColorApplyText_ : std::nullopt;
                flightAntigravityColorApplyText_ = snapshot.HasKey(L"antigravity_accent") ? antigravityColorApplyText_ : std::nullopt;
                flightOpencodeColorApplyText_ = snapshot.HasKey(L"opencode_accent") ? opencodeColorApplyText_ : std::nullopt;
                flightCursorColorApplyText_ = snapshot.HasKey(L"cursor_accent") ? cursorColorApplyText_ : std::nullopt;
                flightGrokColorApplyText_ = snapshot.HasKey(L"grok_accent") ? grokColorApplyText_ : std::nullopt;
                flightCopilotColorApplyText_ = snapshot.HasKey(L"copilot_accent") ? copilotColorApplyText_ : std::nullopt;
                flightUsageTextColorApplyText_ = snapshot.HasKey(L"usage_text_color") ? usageTextColorApplyText_ : std::nullopt;
                if (flightClaudeColorApplyText_) claudeColorApplyText_.reset();
                if (flightCodexColorApplyText_) codexColorApplyText_.reset();
                if (flightAntigravityColorApplyText_) antigravityColorApplyText_.reset();
                if (flightOpencodeColorApplyText_) opencodeColorApplyText_.reset();
                if (flightCursorColorApplyText_) cursorColorApplyText_.reset();
                if (flightGrokColorApplyText_) grokColorApplyText_.reset();
                if (flightCopilotColorApplyText_) copilotColorApplyText_.reset();
                if (flightUsageTextColorApplyText_) usageTextColorApplyText_.reset();
                mutationInFlight_ = true;
                UpdateSaveBar();
                ShowMessage(L"Applying changes…");
                Send(request);
                return;
            }
        }
        if (persistNeeded_ && flushRequested_)
        {
            if (closeRequested_ && closeSaveDecisionPending_ && !closeWithoutSavingApproved_)
            {
                flushRequested_ = false;
                ContinueDeferredWork();
                return;
            }
            if (closeRequested_ && closeWithoutSavingApproved_)
            {
                flushRequested_ = false;
                ContinueDeferredWork();
                return;
            }
            mutationInFlight_ = false;
            auto request = Request(L"persist");
            persistInFlight_ = true;
            UpdateSaveBar();
            ShowMessage(L"Saving changes…");
            Send(request);
            return;
        }
        if (flushRequested_) { flushRequested_ = false; ContinueDeferredWork(); }
        RefreshSettingsControlsIfSafe();
    }

    void MainWindow::ContinueDeferredWork()
    {
#if QUOTA_DEVELOPMENT_UI
        SendNextDevelopmentRequest();
        if (developmentRequestInFlight_) return;
#endif
        const bool canStartDeferredAction = deferredAction_ && !HasPendingChanges() && inFlight_ == 0 &&
            !mutationInFlight_ && !persistInFlight_ &&
            (!persistNeeded_ || (closeRequested_ && closeWithoutSavingApproved_));
        if (canStartDeferredAction && (!closeRequested_ || IsSettingsChangingAction(deferredAction_)))
        {
            auto action = *deferredAction_; deferredAction_.reset();
            settingsActionInFlight_ = IsSettingsChangingAction(action);
            settingsActionInFlightName_ = settingsActionInFlight_ ? std::optional<std::wstring>(action) : std::nullopt;
            if (action == L"reset_colors")
            {
                resetColorsClaudeDraftRevision_ = claudeColorDraftRevision_;
                resetColorsCodexDraftRevision_ = codexColorDraftRevision_;
                resetColorsAntigravityDraftRevision_ = antigravityColorDraftRevision_;
                resetColorsOpencodeDraftRevision_ = opencodeColorDraftRevision_;
                resetColorsCursorDraftRevision_ = cursorColorDraftRevision_;
                resetColorsGrokDraftRevision_ = grokColorDraftRevision_;
                resetColorsCopilotDraftRevision_ = copilotColorDraftRevision_;
                resetColorsUsageTextDraftRevision_ = usageTextColorDraftRevision_;
            }
            auto request = Request(L"action"); request.Insert(L"action", JsonValue::CreateStringValue(action)); Send(request);
            return;
        }

        if (closeRequested_)
        {
            if (closeSaveDecisionDialogOpen_ || HasPendingChanges() || inFlight_ != 0 ||
                mutationInFlight_ || persistInFlight_ || settingsActionInFlight_) return;
            if (HasUnappliedColorDraft())
            {
                closeRequested_ = false;
                closeSaveDecisionPending_ = false;
                closeWithoutSavingApproved_ = false;
                ShowMessage(L"Apply or discard each color draft before closing. Your draft text is still available on the Appearance page.", true);
                return;
            }
            if (persistNeeded_ && !closeWithoutSavingApproved_)
            {
                closeSaveDecisionPending_ = true;
                ShowCloseSaveFailureDialog();
                return;
            }
            closeRequested_ = false;
            closeSaveDecisionPending_ = false;
            closeWithoutSavingApproved_ = false;
            closingFromParent_ = true;
            CloseIfOpen();
            return;
        }
        if (deferredAction_ && !persistNeeded_ && !HasPendingChanges() && inFlight_ == 0)
        {
            auto action = *deferredAction_; deferredAction_.reset();
            settingsActionInFlight_ = IsSettingsChangingAction(action);
            settingsActionInFlightName_ = settingsActionInFlight_ ? std::optional<std::wstring>(action) : std::nullopt;
            if (action == L"reset_colors")
            {
                resetColorsClaudeDraftRevision_ = claudeColorDraftRevision_;
                resetColorsCodexDraftRevision_ = codexColorDraftRevision_;
                resetColorsAntigravityDraftRevision_ = antigravityColorDraftRevision_;
                resetColorsOpencodeDraftRevision_ = opencodeColorDraftRevision_;
                resetColorsCursorDraftRevision_ = cursorColorDraftRevision_;
                resetColorsGrokDraftRevision_ = grokColorDraftRevision_;
                resetColorsCopilotDraftRevision_ = copilotColorDraftRevision_;
                resetColorsUsageTextDraftRevision_ = usageTextColorDraftRevision_;
            }
            auto request = Request(L"action"); request.Insert(L"action", JsonValue::CreateStringValue(action)); Send(request);
        }
    }

    void MainWindow::ShowCloseSaveFailureDialog()
    {
        if (closeSaveDecisionDialogOpen_) return;
        auto root = Navigation().XamlRoot();
        if (!root)
        {
            closeRequested_ = false;
            closeSaveDecisionPending_ = false;
            ShowMessage(L"The save decision could not be opened. Settings remain open; your current changes are still active but may not be saved.", true);
            return;
        }

        auto dialog = ContentDialog();
        dialog.XamlRoot(root);
        dialog.Title(box_value(NativeLocalization::Translate(L"Changes could not be saved", g_nativeLanguage)));
        dialog.Content(box_value(NativeLocalization::Translate(L"Your changes are active in the widget but could not be saved. Retry to save them. Close without saving keeps the current values only for this run; they may be lost when Quota Usage exits. Cancel returns to settings.", g_nativeLanguage)));
        dialog.PrimaryButtonText(NativeLocalization::Translate(L"Retry save", g_nativeLanguage));
        dialog.SecondaryButtonText(NativeLocalization::Translate(L"Close without saving", g_nativeLanguage));
        dialog.CloseButtonText(NativeLocalization::Translate(L"Cancel", g_nativeLanguage));
        dialog.DefaultButton(ContentDialogButton::Primary);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(dialog, L"CloseSaveFailureDialog");
        closeSaveFailureDialog_ = dialog;
        closeSaveDecisionDialogOpen_ = true;

        auto weak = get_weak();
        try
        {
            auto operation = dialog.ShowAsync();
            operation.Completed([weak](auto const& completedOperation, auto const& status)
            {
                auto result = ContentDialogResult::None;
                const bool completed = status == Windows::Foundation::AsyncStatus::Completed;
                if (completed)
                {
                    try { result = completedOperation.GetResults(); }
                    catch (...) {}
                }
                if (auto self = weak.get())
                {
                    self->dispatcher_.TryEnqueue([weak, result, completed]()
                    {
                        if (auto owner = weak.get()) owner->CompleteCloseSaveDecision(result, completed);
                    });
                }
            });
        }
        catch (...)
        {
            closeSaveFailureDialog_ = nullptr;
            closeSaveDecisionDialogOpen_ = false;
            closeRequested_ = false;
            closeSaveDecisionPending_ = false;
            ShowMessage(L"The save decision could not be opened. Settings remain open; your current changes are still active but may not be saved.", true);
        }
    }

    void MainWindow::CompleteCloseSaveDecision(ContentDialogResult result, bool completed)
    {
        if (closed_) return;
        closeSaveFailureDialog_ = nullptr;
        closeSaveDecisionDialogOpen_ = false;
        if (!completed)
        {
            closeRequested_ = false;
            closeSaveDecisionPending_ = false;
            closeWithoutSavingApproved_ = false;
            ShowMessage(L"The save decision could not be completed. Settings remain open; your current changes may not be saved.", true);
            return;
        }

        if (result == ContentDialogResult::Primary)
        {
            closeRequested_ = true;
            closeSaveDecisionPending_ = false;
            closeWithoutSavingApproved_ = false;
            FlushPending();
            return;
        }
        if (result == ContentDialogResult::Secondary)
        {
            closeRequested_ = true;
            closeSaveDecisionPending_ = false;
            closeWithoutSavingApproved_ = true;
            FlushPending();
            return;
        }

        closeRequested_ = false;
        closeSaveDecisionPending_ = false;
        closeWithoutSavingApproved_ = false;
        ShowMessage(IsSettingsChangingAction(deferredAction_)
            ? L"Close canceled. The requested settings action is still waiting for a successful save. Current changes remain active; unsaved changes may be lost when Quota Usage exits."
            : L"Close canceled. Current changes remain active; unsaved changes may be lost when Quota Usage exits.", true);
    }

    void MainWindow::OnWindowClosing(Microsoft::UI::Windowing::AppWindow const&, Microsoft::UI::Windowing::AppWindowClosingEventArgs const& args)
    {
        if (closingFromParent_ || stopping_) return;
        const bool hasCloseSensitiveAction = settingsActionInFlight_ || IsSettingsChangingAction(deferredAction_);
        const bool hasPendingSave = HasPendingChanges() || mutationInFlight_ || persistInFlight_ || persistNeeded_;
        const bool hasUnappliedColorDraft = HasUnappliedColorDraft();
        if (!hasCloseSensitiveAction && !hasPendingSave && !hasUnappliedColorDraft) return;
        args.Cancel(true);
        if (closeRequested_) return;
        if (hasUnappliedColorDraft)
        {
            ShowMessage(L"Apply or discard each color draft before closing. Your draft text is still available on the Appearance page.", true);
            return;
        }
        closeRequested_ = true;
        FlushPending();
    }

    void MainWindow::Receive(JsonObject const& message)
    {
        if (closed_) return;
        if (message.HasKey(L"event"))
        {
            auto event = message.GetNamedString(L"event", L"");
            if (event == L"shutdown") { stopping_ = true; closingFromParent_ = true; CloseIfOpen(); return; }
            if (event == L"activate")
            {
                auto target = std::wstring(message.GetNamedString(L"page", L"general").c_str());
                if (!target.empty()) target[0] = static_cast<wchar_t>(towupper(target[0]));
#if QUOTA_DEVELOPMENT_UI
                if (target == L"Development") OpenDevelopment();
#endif
                for (auto const& item : Navigation().MenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == target) Navigation().SelectedItem(item);
                for (auto const& item : Navigation().FooterMenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == target) Navigation().SelectedItem(item);
                if (IsIconic(hwnd_)) ShowWindow(hwnd_, SW_RESTORE);
                Activate();
                SetForegroundWindow(hwnd_);
            }
            else if (event == L"state" && message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object)
            {
                auto updated = message.GetNamedObject(L"state");
                const auto revision = static_cast<uint64_t>(message.GetNamedNumber(L"revision", 0));
                bool settingsChanged = false;
                bool localEcho = false;
                if (revision >= revision_)
                {
#if QUOTA_DEVELOPMENT_UI
                    if (page_ == L"Appearance" && state_ &&
                        state_.GetNamedObject(L"development_overrides", JsonObject()).Stringify() !=
                        updated.GetNamedObject(L"development_overrides", JsonObject()).Stringify())
                        settingsRefreshPending_ = true;
#endif
                    settingsChanged = SettingsChanged(state_, updated);
                    auto localChanges = JsonObject();
                    if (flightChanges_) for (auto const& pair : flightChanges_) localChanges.Insert(pair.Key(), pair.Value());
                    if (pendingChanges_) for (auto const& pair : pendingChanges_) if (!localChanges.HasKey(pair.Key())) localChanges.Insert(pair.Key(), pair.Value());
                    localEcho = settingsChanged && SettingsChangesAreLocalEcho(state_, updated, localChanges);
                    state_ = updated;
                    revision_ = revision;
                    UpdateWidgetPreview(updated);
                    UpdateWidgetPageAvailability();
                }
                UpdateProviderAvailability();
                UpdateAboutControls();
                if (settingsChanged) SyncPaneState();
                // Poll/status events are frequent; avoid rebuilding controls (and
                // stealing keyboard focus) when only non-setting state changed.
                // Local edits are already represented by the controls and the
                // draft overlay. Recreating the page here makes edits lose focus.
                bool refreshPage = std::wstring(updated.GetNamedString(L"effective_language", L"en").c_str()) != g_nativeLanguage;
                if (settingsChanged && !localEcho)
                {
                    settingsRefreshPending_ = true;
                    refreshPage = true;
                }
                if (refreshPage) settingsRefreshPending_ = true;
                if (refreshPage) RefreshSettingsControlsIfSafe();
#if QUOTA_DEVELOPMENT_UI
                if (page_ == L"Appearance") RefreshSettingsControlsIfSafe();
#endif
            }
            return;
        }
        if (!message.HasKey(L"id") || message.GetNamedNumber(L"id", 0) != inFlight_) return;
        const bool isInitialGet = initialGetPending_;
        const bool wasPreview = previewInFlight_;
        const auto completedPreviewGeneration = widgetPreviewFlightGeneration_;
        const bool wasPresetPreview = presetPreviewInFlight_;
        const auto completedPresetGeneration = appearancePresetFlightGeneration_;
        const auto completedPresetIndex = appearancePresetFlightIndex_;
        const bool wasMutation = mutationInFlight_;
        const bool wasPanePersistence = paneStateRequestInFlight_;
        const bool wasSettingsAction = settingsActionInFlight_;
#if QUOTA_DEVELOPMENT_UI
        const bool wasDevelopment = developmentRequestInFlight_;
        const bool wasDevelopmentReset = developmentResetInFlight_;
        developmentRequestInFlight_ = false;
        developmentResetInFlight_ = false;
#endif
        const auto completedSettingsAction = settingsActionInFlightName_;
        initialGetPending_ = false;
        previewInFlight_ = false;
        presetPreviewInFlight_ = false;
        if (wasPanePersistence)
        {
            paneStateRequestInFlight_ = false;
            paneStateRequestValue_.reset();
        }
        settingsActionInFlight_ = false;
        settingsActionInFlightName_.reset();
        inFlight_ = 0;
        const bool ok = message.GetNamedBoolean(L"ok", false);
#if QUOTA_DEVELOPMENT_UI
        if (wasDevelopment && !ok)
            ShowMessage(std::wstring(message.GetNamedString(L"error", L"The development override could not be applied.").c_str()), true);
#endif
        if (wasPreview || wasPresetPreview)
        {
            if (wasPresetPreview && ok && completedPresetGeneration == appearancePresetGeneration_ &&
                page_ == L"Appearance" && completedPresetIndex < appearancePresetImages_.size() &&
                message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object)
            {
                auto snapshot = message.GetNamedObject(L"state");
                const auto image = snapshot.GetNamedString(L"widget_preview_image", L"");
                if (!image.empty()) LoadPresetPreview(std::wstring(image.c_str()), completedPresetGeneration, completedPresetIndex);
                else if (completedPresetIndex < appearancePresetStatuses_.size())
                    appearancePresetStatuses_[completedPresetIndex].Text(NativeLocalization::Translate(L"Preview unavailable", g_nativeLanguage));
            }
            else if (wasPresetPreview && completedPresetGeneration == appearancePresetGeneration_ &&
                completedPresetIndex < appearancePresetStatuses_.size())
                appearancePresetStatuses_[completedPresetIndex].Text(NativeLocalization::Translate(L"Preview unavailable", g_nativeLanguage));
            else if (wasPreview && ok && completedPreviewGeneration == widgetPreviewGeneration_ &&
                message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object)
                UpdateWidgetPreview(message.GetNamedObject(L"state"));
            else if (wasPreview && !ok && completedPreviewGeneration == widgetPreviewGeneration_ && widgetPreviewStatus_)
                widgetPreviewStatus_.Text(NativeLocalization::Translate(L"The widget preview is temporarily unavailable.", g_nativeLanguage));

            SendPendingPaneState();
#if QUOTA_DEVELOPMENT_UI
            SendNextDevelopmentRequest();
#endif
            if (flushRequested_) SendNextMutation();
            else if (appearancePresetPreviewNext_ < appearancePresetPreviewJobs_.size()) SendNextPresetPreviewIfIdle();
            else if (previewPending_) SendWidgetPreviewIfIdle();
            else ContinueDeferredWork();
            RefreshSettingsControlsIfSafe();
            return;
        }
        const auto responseRevision = static_cast<uint64_t>(message.GetNamedNumber(L"revision", 0));
        if (message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object && responseRevision >= revision_)
        {
            state_ = message.GetNamedObject(L"state"); revision_ = responseRevision; UpdateWidgetPreview(state_); UpdateWidgetPageAvailability();
            if (std::wstring(state_.GetNamedString(L"effective_language", L"en").c_str()) != g_nativeLanguage) settingsRefreshPending_ = true;
        }
        if (isInitialGet)
        {
            SyncPaneState();
        }
        UpdateProviderAvailability();
        UpdateAboutControls();
#if QUOTA_DEVELOPMENT_UI
        if (wasDevelopment && ok)
        {
            ShowMessage(L"Development overrides updated. Saved settings are unchanged.");
            if (wasDevelopmentReset && page_ == L"Development") RenderPage(page_);
        }
#endif
        const bool wasPersist = persistInFlight_;
        persistInFlight_ = false;
        if (mutationInFlight_)
        {
            mutationInFlight_ = false;
            if (!ok && message.GetNamedString(L"error_code", L"") == L"save_failed")
            {
                persistNeeded_ = true;
                if (flightClaudeColorApplyText_) persistClaudeColorApplyText_ = flightClaudeColorApplyText_;
                if (flightCodexColorApplyText_) persistCodexColorApplyText_ = flightCodexColorApplyText_;
                if (flightAntigravityColorApplyText_) persistAntigravityColorApplyText_ = flightAntigravityColorApplyText_;
                if (flightOpencodeColorApplyText_) persistOpencodeColorApplyText_ = flightOpencodeColorApplyText_;
                if (flightCursorColorApplyText_) persistCursorColorApplyText_ = flightCursorColorApplyText_;
                if (flightGrokColorApplyText_) persistGrokColorApplyText_ = flightGrokColorApplyText_;
                if (flightCopilotColorApplyText_) persistCopilotColorApplyText_ = flightCopilotColorApplyText_;
                if (flightUsageTextColorApplyText_) persistUsageTextColorApplyText_ = flightUsageTextColorApplyText_;
                ShowMessage(std::wstring(message.GetNamedString(L"error", L"Changes are active, but could not be saved.").c_str()), true);
            }
            else if (!ok)
            {
                // A rejected batch was not applied. Do not retry it automatically;
                // only edits made after it was sent remain eligible for a new batch.
                flushRequested_ = false;
                const bool keepCloseForSettingsAction = closeRequested_ && IsSettingsChangingAction(deferredAction_);
                if (!keepCloseForSettingsAction) deferredAction_.reset();
                closeRequested_ = keepCloseForSettingsAction;
                closeSaveDecisionPending_ = false;
                closeWithoutSavingApproved_ = false;
                flightClaudeColorApplyText_.reset();
                flightCodexColorApplyText_.reset();
                flightAntigravityColorApplyText_.reset();
                flightOpencodeColorApplyText_.reset();
                flightCursorColorApplyText_.reset();
                flightGrokColorApplyText_.reset();
                flightCopilotColorApplyText_.reset();
                flightUsageTextColorApplyText_.reset();
                ShowMessage(std::wstring(message.GetNamedString(L"error", L"The setting could not be applied.").c_str()), true);
            }
            else
            {
                persistNeeded_ = false;
                closeSaveDecisionPending_ = false;
                CompleteColorApply(L"claude_accent", flightClaudeColorApplyText_);
                CompleteColorApply(L"codex_accent", flightCodexColorApplyText_);
                CompleteColorApply(L"antigravity_accent", flightAntigravityColorApplyText_);
                CompleteColorApply(L"opencode_accent", flightOpencodeColorApplyText_);
                CompleteColorApply(L"cursor_accent", flightCursorColorApplyText_);
                CompleteColorApply(L"grok_accent", flightGrokColorApplyText_);
                CompleteColorApply(L"copilot_accent", flightCopilotColorApplyText_);
                CompleteColorApply(L"usage_text_color", flightUsageTextColorApplyText_);
                CompleteColorApply(L"claude_accent", persistClaudeColorApplyText_);
                CompleteColorApply(L"codex_accent", persistCodexColorApplyText_);
                CompleteColorApply(L"antigravity_accent", persistAntigravityColorApplyText_);
                CompleteColorApply(L"opencode_accent", persistOpencodeColorApplyText_);
                CompleteColorApply(L"cursor_accent", persistCursorColorApplyText_);
                CompleteColorApply(L"grok_accent", persistGrokColorApplyText_);
                CompleteColorApply(L"copilot_accent", persistCopilotColorApplyText_);
                CompleteColorApply(L"usage_text_color", persistUsageTextColorApplyText_);
                flightClaudeColorApplyText_.reset();
                flightCodexColorApplyText_.reset();
                flightAntigravityColorApplyText_.reset();
                flightOpencodeColorApplyText_.reset();
                flightCursorColorApplyText_.reset();
                flightGrokColorApplyText_.reset();
                flightCopilotColorApplyText_.reset();
                flightUsageTextColorApplyText_.reset();
                persistClaudeColorApplyText_.reset();
                persistCodexColorApplyText_.reset();
                persistAntigravityColorApplyText_.reset();
                persistOpencodeColorApplyText_.reset();
                persistCursorColorApplyText_.reset();
                persistGrokColorApplyText_.reset();
                persistCopilotColorApplyText_.reset();
                persistUsageTextColorApplyText_.reset();
                if (!isInitialGet) ShowMessage(L"Saved. Changes are now in effect.");
            }
            flightChanges_ = nullptr;
        }
        else if (wasPersist)
        {
            if (ok)
            {
                persistNeeded_ = false;
                closeSaveDecisionPending_ = false;
                CompleteColorApply(L"claude_accent", persistClaudeColorApplyText_);
                CompleteColorApply(L"codex_accent", persistCodexColorApplyText_);
                CompleteColorApply(L"antigravity_accent", persistAntigravityColorApplyText_);
                CompleteColorApply(L"opencode_accent", persistOpencodeColorApplyText_);
                CompleteColorApply(L"cursor_accent", persistCursorColorApplyText_);
                CompleteColorApply(L"grok_accent", persistGrokColorApplyText_);
                CompleteColorApply(L"copilot_accent", persistCopilotColorApplyText_);
                CompleteColorApply(L"usage_text_color", persistUsageTextColorApplyText_);
                persistClaudeColorApplyText_.reset();
                persistCodexColorApplyText_.reset();
                persistAntigravityColorApplyText_.reset();
                persistOpencodeColorApplyText_.reset();
                persistCursorColorApplyText_.reset();
                persistGrokColorApplyText_.reset();
                persistCopilotColorApplyText_.reset();
                persistUsageTextColorApplyText_.reset();
                ShowMessage(L"Changes saved.");
            }
            else
            {
                persistNeeded_ = true;
                flushRequested_ = false;
                if (closeRequested_)
                {
                    closeSaveDecisionPending_ = true;
                    if (HasPendingChanges()) flushRequested_ = true;
                }
                ShowMessage(std::wstring(message.GetNamedString(L"error", L"Changes could not be saved. Try again.").c_str()), true);
            }
        }
        else if (wasPanePersistence && !ok)
        {
            ShowMessage(std::wstring(message.GetNamedString(L"error", L"The navigation layout could not be saved.").c_str()), true);
            if (!pendingPaneState_) SyncPaneState();
        }
        const bool settingsActionSaveFailed = wasSettingsAction && !ok &&
            message.GetNamedString(L"error_code", L"") == L"save_failed";
        if (settingsActionSaveFailed)
        {
            persistNeeded_ = true;
            settingsRefreshPending_ = true;
            if (completedSettingsAction == L"reset_colors")
                ClearResetColorDraftsIfUnchanged(resetColorsClaudeDraftRevision_, resetColorsCodexDraftRevision_, resetColorsAntigravityDraftRevision_, resetColorsOpencodeDraftRevision_, resetColorsCursorDraftRevision_, resetColorsGrokDraftRevision_, resetColorsCopilotDraftRevision_, resetColorsUsageTextDraftRevision_);
            if (closeRequested_) flushRequested_ = true;
            ShowMessage(std::wstring(message.GetNamedString(L"error", L"The action took effect, but its changes could not be saved.").c_str()), true);
        }
        else if (!ok && !wasMutation && !wasPersist && !wasPanePersistence) ShowMessage(std::wstring(message.GetNamedString(L"error", L"The request could not be completed.").c_str()), true);
        if (wasSettingsAction && !ok && !settingsActionSaveFailed)
        {
            closeRequested_ = false;
            closeSaveDecisionPending_ = false;
            closeWithoutSavingApproved_ = false;
        }
        if (isInitialGet)
        {
            Hydrate();
            settingsRefreshPending_ = false;
        }
        else if (wasMutation && !ok)
        {
            SyncPaneState();
            settingsRefreshPending_ = true;
        }
        else if (wasSettingsAction && ok)
        {
            settingsRefreshPending_ = true;
            if (completedSettingsAction == L"reset_colors")
                ClearResetColorDraftsIfUnchanged(resetColorsClaudeDraftRevision_, resetColorsCodexDraftRevision_, resetColorsAntigravityDraftRevision_, resetColorsOpencodeDraftRevision_, resetColorsCursorDraftRevision_, resetColorsGrokDraftRevision_, resetColorsCopilotDraftRevision_, resetColorsUsageTextDraftRevision_);
        }
        if (HasPendingChanges() && flushRequested_) SendNextMutation();
        else if (flushRequested_) SendNextMutation();
        else ContinueDeferredWork();
        SendPendingPaneState();
        if (flushRequested_) SendNextMutation();
        else if (appearancePresetPreviewNext_ < appearancePresetPreviewJobs_.size()) SendNextPresetPreviewIfIdle();
        else if (previewPending_) SendWidgetPreviewIfIdle();
        RefreshSettingsControlsIfSafe();
        UpdateSaveBar();
    }

    JsonObject MainWindow::State() const
    {
        auto settings = state_ ? state_.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        auto effective = JsonObject::Parse(settings.Stringify());
        if (flightChanges_) for (auto const& pair : flightChanges_) effective.Insert(pair.Key(), pair.Value());
        if (pendingChanges_) for (auto const& pair : pendingChanges_) effective.Insert(pair.Key(), pair.Value());
        return effective;
    }
    JsonObject MainWindow::Setting(std::wstring const&) { return State(); }
    bool MainWindow::Bool(std::wstring const& key) { auto s = State(); return s.GetNamedBoolean(key, false); }
    bool MainWindow::AppearanceProviderEnabled(std::wstring const& key)
    {
        if (Bool(key)) return true;
#if QUOTA_DEVELOPMENT_UI
        if (state_ && state_.GetNamedBoolean(L"development_enabled", false))
        {
            const auto id = key == L"show_claude_code" ? std::wstring(L"claude") : key.substr(5);
            const auto overrides = state_.GetNamedObject(L"development_overrides", JsonObject());
            const auto provider = overrides.GetNamedObject(id, JsonObject());
            return provider.GetNamedBoolean(L"enabled", false) && provider.GetNamedBoolean(L"force_available", false);
        }
#endif
        return false;
    }
    std::wstring MainWindow::Text(std::wstring const& key)
    {
        auto settings = State();
        if (!settings.HasKey(key) || settings.GetNamedValue(key).ValueType() != JsonValueType::String) return {};
        return std::wstring(settings.GetNamedString(key).c_str());
    }
    void MainWindow::SetValue(std::wstring const& key, IJsonValue const& value)
    {
        if (hydrating_) return;
        if (!pendingChanges_) pendingChanges_ = JsonObject();
        pendingChanges_.Insert(key, value);
        UpdateProviderAvailability();
        flushRequested_ = false;
        UpdateSaveBar();
        if (key == L"widget_visible")
        {
            UpdateWidgetPageAvailability();
            if (page_ == L"Appearance" || page_ == L"Placement") RenderPage(page_);
        }
        if (key == L"palette" || key == L"bar_style" || key == L"bar_thickness" || key == L"provider_spacing" || key == L"font_size" ||
            key == L"provider_icon_position" || key == L"provider_order" || key == L"provider_monochrome" ||
            key == L"window_label_font_size" || key == L"claude_accent" || key == L"codex_accent" ||
            key == L"antigravity_accent" || key == L"opencode_accent" || key == L"cursor_accent" ||
            key == L"grok_accent" || key == L"copilot_accent" ||
            key == L"usage_text_color" || key == L"show_provider_icon" || key == L"show_provider_icons" ||
            key == L"show_claude_icon" || key == L"show_codex_icon" || key == L"show_antigravity_icon" ||
            key == L"show_opencode_icon" || key == L"show_cursor_icon" || key == L"show_grok_icon" ||
            key == L"show_copilot_icon" ||
            key == L"show_percentage" || key == L"show_bar" || key == L"show_reset_time")
            ScheduleWidgetPreview();
    }
    void MainWindow::ApplyAppearancePreset(JsonObject const& target)
    {
        // Keep the live preview and scroll host mounted; rebuilding this page
        // invalidates its image load and can bring the clicked preset into view.
        const auto currentSettings = State();
        if (!pendingChanges_) pendingChanges_ = JsonObject();
        bool changed = false;
        for (auto const& pair : target)
        {
            if (!currentSettings.HasKey(pair.Key()) ||
                currentSettings.GetNamedValue(pair.Key()).Stringify() != pair.Value().Stringify())
            {
                pendingChanges_.Insert(pair.Key(), pair.Value());
                changed = true;
            }
        }
        if (!changed) return;

        flushRequested_ = false;
        UpdateSaveBar();
        const bool wasHydrating = hydrating_.load();
        const bool wasSyncingColorText = syncingColorText_;
        hydrating_ = true;
        syncingColorText_ = true;

        for (auto const& [key, toggle] : appearancePresetToggles_)
            if (target.HasKey(key)) toggle.IsOn(target.GetNamedBoolean(key, toggle.IsOn()));

        for (auto const& [key, combo] : appearancePresetCombos_)
        {
            if (!target.HasKey(key)) continue;
            const auto value = target.GetNamedValue(key);
            for (uint32_t index = 0; index < combo.Items().Size(); ++index)
            {
                const auto item = combo.Items().GetAt(index).try_as<ComboBoxItem>();
                if (!item || !item.Tag()) continue;
                const auto tag = item.Tag().try_as<Windows::Foundation::IPropertyValue>();
                if (!tag) continue;
                bool matches = false;
                if (value.ValueType() == JsonValueType::String && tag.Type() == Windows::Foundation::PropertyType::String)
                    matches = std::wstring(unbox_value<hstring>(item.Tag()).c_str()) == std::wstring(value.GetString().c_str());
                else if (value.ValueType() == JsonValueType::Number && tag.Type() == Windows::Foundation::PropertyType::Int32)
                    matches = unbox_value<int32_t>(item.Tag()) == static_cast<int32_t>(value.GetNumber());
                if (matches)
                {
                    combo.SelectedIndex(index);
                    break;
                }
            }
        }

        for (auto const& [key, input] : appearancePresetColorInputs_)
            input.Text(StoredColorText(State(), key));
        if (providerIconPositionSelector_)
            providerIconPositionSelector_.IsEnabled(target.GetNamedBoolean(L"show_provider_icon", true));

        if (target.HasKey(L"provider_order") && providerOrderGrid_ &&
            target.GetNamedValue(L"provider_order").ValueType() == JsonValueType::Array)
        {
            providerFullOrder_.clear();
            for (auto const& value : target.GetNamedArray(L"provider_order"))
                if (value.ValueType() == JsonValueType::String)
                    providerFullOrder_.emplace_back(value.GetString().c_str());

            std::vector<std::pair<std::wstring, Border>> tiles;
            for (auto const& item : providerOrderGrid_.Items())
            {
                auto tile = item.try_as<Border>();
                if (tile && tile.Tag())
                    tiles.emplace_back(unbox_value<hstring>(tile.Tag()).c_str(), tile);
            }
            providerOrderGrid_.Items().Clear();
            for (auto const& id : providerFullOrder_)
            {
                const auto found = std::find_if(tiles.begin(), tiles.end(), [&id](auto const& entry) { return entry.first == id; });
                if (found != tiles.end())
                {
                    providerOrderGrid_.Items().Append(found->second);
                    tiles.erase(found);
                }
            }
            for (auto const& [id, tile] : tiles)
                providerOrderGrid_.Items().Append(tile);

            uint32_t position = 0;
            for (auto const& item : providerOrderGrid_.Items())
            {
                auto tile = item.try_as<Border>();
                auto content = tile ? tile.Child().try_as<Grid>() : Grid(nullptr);
                if (content && content.Children().Size() > 1)
                    content.Children().GetAt(1).as<TextBlock>().Text(std::to_wstring(++position));
            }
        }

        syncingColorText_ = wasSyncingColorText;
        hydrating_ = wasHydrating;
        ScheduleWidgetPreview();
    }
    void MainWindow::Action(std::wstring const& action)
    {
        if (previewTimer_) previewTimer_.Stop();
        previewPending_ = false;
        deferredAction_ = action;
        FlushPending();
    }
    void MainWindow::ShowMessage(std::wstring const& text, bool error)
    {
        MessageBar().Message(NativeLocalization::TranslateMessage(text, g_nativeLanguage));
        MessageBar().Severity(error ? InfoBarSeverity::Error : InfoBarSeverity::Informational);
        MessageBar().IsOpen(true);
        UpdateToastPosition();
        if (messageTimer_) { messageTimer_.Stop(); messageTimer_.Start(); }
    }
    void MainWindow::ScheduleWidgetPreview()
    {
        if (closed_ || page_ != L"Appearance") return;
        ++widgetPreviewGeneration_;
        ++widgetPreviewImageGeneration_;
        previewPending_ = true;
        if (previewTimer_) { previewTimer_.Stop(); previewTimer_.Start(); }
    }
    void MainWindow::SendWidgetPreviewIfIdle()
    {
        if (closed_ || stopping_ || page_ != L"Appearance" || inFlight_ != 0) return;
        if (appearancePresetPreviewNext_ < appearancePresetPreviewJobs_.size())
        {
            SendNextPresetPreviewIfIdle();
            return;
        }
        if (!previewPending_) return;
        if (previewTimer_) previewTimer_.Stop();
        auto settings = State();
        auto changes = JsonObject();
        static const std::array<std::wstring, 28> previewKeys{
            L"palette", L"bar_style", L"bar_thickness", L"provider_spacing", L"font_size", L"window_label_font_size",
            L"claude_accent", L"codex_accent", L"antigravity_accent", L"opencode_accent",
            L"cursor_accent", L"grok_accent", L"copilot_accent", L"usage_text_color", L"provider_monochrome", L"provider_order",
            L"show_provider_icon", L"provider_icon_position",
            L"show_claude_icon", L"show_codex_icon", L"show_antigravity_icon", L"show_opencode_icon",
            L"show_cursor_icon", L"show_grok_icon", L"show_copilot_icon",
            L"show_percentage", L"show_bar", L"show_reset_time" };
        for (auto const& key : previewKeys)
            if (settings.HasKey(key)) changes.Insert(key, settings.GetNamedValue(key));
        auto addColorDraft = [&changes](std::wstring const& key, std::optional<std::wstring> const& draft)
        {
            if (!draft) return;
            const auto rgb = ParseHexColor(*draft);
            if (!rgb) return;
            auto array = JsonArray();
            for (const auto component : *rgb) array.Append(JsonValue::CreateNumberValue(component));
            changes.Insert(key, array);
        };
        addColorDraft(L"claude_accent", claudeColorDraft_);
        addColorDraft(L"codex_accent", codexColorDraft_);
        addColorDraft(L"antigravity_accent", antigravityColorDraft_);
        addColorDraft(L"opencode_accent", opencodeColorDraft_);
        addColorDraft(L"cursor_accent", cursorColorDraft_);
        addColorDraft(L"grok_accent", grokColorDraft_);
        addColorDraft(L"copilot_accent", copilotColorDraft_);
        addColorDraft(L"usage_text_color", usageTextColorDraft_);

        auto request = Request(L"preview");
        request.Insert(L"changes", changes);
        widgetPreviewFlightGeneration_ = widgetPreviewGeneration_;
        previewInFlight_ = true;
        previewPending_ = false;
        Send(request);
        if (inFlight_ == 0) previewInFlight_ = false;
    }

    void MainWindow::SendNextPresetPreviewIfIdle()
    {
        if (closed_ || stopping_ || page_ != L"Appearance" || inFlight_ != 0 ||
            presetPreviewInFlight_ || appearancePresetPreviewNext_ >= appearancePresetPreviewJobs_.size()) return;
        const auto jobIndex = appearancePresetPreviewNext_++;
        const auto index = appearancePresetPreviewJobs_[jobIndex].cardIndex;
        auto request = Request(L"preview");
        request.Insert(L"changes", appearancePresetPreviewJobs_[jobIndex].changes);
        appearancePresetFlightGeneration_ = appearancePresetGeneration_;
        appearancePresetFlightIndex_ = index;
        presetPreviewInFlight_ = true;
        Send(request);
        if (inFlight_ == 0) presetPreviewInFlight_ = false;
    }

    winrt::fire_and_forget MainWindow::LoadPresetPreview(std::wstring imageBase64,
        uint64_t generation, uint32_t index)
    {
        auto weak = get_weak();
        try
        {
            constexpr wchar_t dataPrefix[] = L"data:image/bmp;base64,";
            if (imageBase64.rfind(dataPrefix, 0) == 0) imageBase64.erase(0, (sizeof(dataPrefix) / sizeof(dataPrefix[0])) - 1);
            auto buffer = Windows::Security::Cryptography::CryptographicBuffer::DecodeFromBase64String(imageBase64);
            Windows::Storage::Streams::InMemoryRandomAccessStream stream;
            Windows::Storage::Streams::DataWriter writer(stream);
            writer.WriteBuffer(buffer);
            co_await writer.StoreAsync();
            writer.DetachStream();
            stream.Seek(0);
            Media::Imaging::BitmapImage bitmap;
            co_await bitmap.SetSourceAsync(stream);
            if (auto self = weak.get(); self && generation == self->appearancePresetGeneration_ &&
                self->page_ == L"Appearance" && index < self->appearancePresetImages_.size())
            {
                auto image = self->appearancePresetImages_[index];
                image.Source(bitmap);
                image.Visibility(Visibility::Visible);
                self->appearancePresetStatuses_[index].Text(NativeLocalization::Translate(L"Widget-rendered preview", g_nativeLanguage));
            }
        }
        catch (...)
        {
            if (auto self = weak.get(); self && generation == self->appearancePresetGeneration_ &&
                self->page_ == L"Appearance" && index < self->appearancePresetStatuses_.size())
                self->appearancePresetStatuses_[index].Text(NativeLocalization::Translate(L"Preview unavailable", g_nativeLanguage));
        }
    }
    void MainWindow::UpdateWidgetPreview(JsonObject const& snapshot)
    {
        if (snapshot)
        {
            const auto width = snapshot.HasKey(L"widget_preview_width") &&
                snapshot.GetNamedValue(L"widget_preview_width").ValueType() == JsonValueType::Number
                ? snapshot.GetNamedNumber(L"widget_preview_width", 0) : 0;
            const auto height = snapshot.HasKey(L"widget_preview_height") &&
                snapshot.GetNamedValue(L"widget_preview_height").ValueType() == JsonValueType::Number
                ? snapshot.GetNamedNumber(L"widget_preview_height", 0) : 0;
            widgetPreviewWidthPx_ = width > 0 && width <= 8192 ? static_cast<uint32_t>(width) : 0;
            widgetPreviewHeightPx_ = height > 0 && height <= 8192 ? static_cast<uint32_t>(height) : 0;
            const auto dpi = snapshot.GetNamedNumber(L"widget_preview_dpi", 96);
            widgetPreviewDpi_ = dpi >= 48 && dpi <= 768 ? static_cast<uint32_t>(dpi) : 96u;
        }
        if (snapshot && snapshot.HasKey(L"widget_preview_image"))
        {
            const auto image = snapshot.GetNamedValue(L"widget_preview_image").ValueType() == JsonValueType::String
                ? std::wstring(snapshot.GetNamedString(L"widget_preview_image").c_str()) : std::wstring();
            if (image != latestWidgetPreviewBase64_)
            {
                latestWidgetPreviewBase64_ = image;
                ++widgetPreviewImageGeneration_;
                displayedWidgetPreviewBase64_.clear();
            }
        }

        if (!widgetPreviewStatus_) return;
        if (latestWidgetPreviewBase64_.empty())
        {
            if (widgetPreviewImage_) widgetPreviewImage_.Visibility(Visibility::Collapsed);
            widgetPreviewStatus_.Text(NativeLocalization::Translate(L"Waiting for the widget’s sample preview…", g_nativeLanguage));
            return;
        }
        if (widgetPreviewImage_ && displayedWidgetPreviewBase64_ != latestWidgetPreviewBase64_)
            LoadWidgetPreview(latestWidgetPreviewBase64_, widgetPreviewImageGeneration_);
    }
    winrt::fire_and_forget MainWindow::LoadWidgetPreview(std::wstring imageBase64, uint64_t generation)
    {
        auto weak = get_weak();
        const auto displayKey = imageBase64;
        try
        {
            constexpr wchar_t dataPrefix[] = L"data:image/bmp;base64,";
            if (imageBase64.rfind(dataPrefix, 0) == 0) imageBase64.erase(0, (sizeof(dataPrefix) / sizeof(dataPrefix[0])) - 1);
            auto buffer = Windows::Security::Cryptography::CryptographicBuffer::DecodeFromBase64String(imageBase64);
            Windows::Storage::Streams::InMemoryRandomAccessStream stream;
            Windows::Storage::Streams::DataWriter writer(stream);
            writer.WriteBuffer(buffer);
            co_await writer.StoreAsync();
            writer.DetachStream();
            stream.Seek(0);
            Media::Imaging::BitmapImage bitmap;
            co_await bitmap.SetSourceAsync(stream);
            if (auto self = weak.get(); self && generation == self->widgetPreviewImageGeneration_ && self->widgetPreviewImage_)
            {
                const auto dpi = self->widgetPreviewDpi_;
                const double scale = dpi ? static_cast<double>(dpi) / 96.0 : 1.0;
                const bool keepPreviewSize = self->widgetPreviewImage_.Visibility() == Visibility::Visible &&
                    self->widgetPreviewImage_.Width() > 0 && self->widgetPreviewImage_.Height() > 0;
                self->widgetPreviewImage_.Source(bitmap);
                // Keep replacement previews inside the existing layout slot so
                // changing widget dimensions cannot move the scrolled content.
                if (!keepPreviewSize)
                {
                    self->widgetPreviewImage_.Width((self->widgetPreviewWidthPx_ ? self->widgetPreviewWidthPx_ : bitmap.PixelWidth()) / scale);
                    self->widgetPreviewImage_.Height((self->widgetPreviewHeightPx_ ? self->widgetPreviewHeightPx_ : bitmap.PixelHeight()) / scale);
                }
                self->widgetPreviewImage_.Visibility(Visibility::Visible);
                self->UpdatePreviewOverflow();
                self->displayedWidgetPreviewBase64_ = displayKey;
                self->widgetPreviewStatus_.Text(NativeLocalization::Translate(L"Sample values rendered by the widget.", g_nativeLanguage));
            }
        }
        catch (...)
        {
            if (auto self = weak.get(); self && generation == self->widgetPreviewImageGeneration_ && self->widgetPreviewStatus_)
            {
                self->widgetPreviewStatus_.Text(NativeLocalization::Translate(L"The widget preview is temporarily unavailable.", g_nativeLanguage));
            }
        }
    }
    struct ProviderAppearanceEntry
    {
        std::wstring enable;
        std::wstring icon;
        std::wstring accent;
        std::wstring name;
        std::wstring availability;
        std::wstring setupName;
        std::wstring monitoringDetail;
    };

    static std::array<ProviderAppearanceEntry, 7> ProviderAppearanceMeta()
    {
        return {{
            {L"show_claude_code", L"show_claude_icon", L"claude_accent", L"Claude Code", L"claude_code_available", L"Claude CLI", L"Monitor usage from your Claude CLI account."},
            {L"show_codex", L"show_codex_icon", L"codex_accent", L"Codex", L"codex_available", L"Codex", L"Monitor your Codex account."},
            {L"show_antigravity", L"show_antigravity_icon", L"antigravity_accent", L"Antigravity", L"antigravity_available", L"Antigravity", L"Monitor your Antigravity account."},
            {L"show_opencode", L"show_opencode_icon", L"opencode_accent", L"OpenCode", L"opencode_available", L"OpenCode Go", L"Monitor your OpenCode Go usage."},
            {L"show_cursor", L"show_cursor_icon", L"cursor_accent", L"Cursor", L"cursor_available", L"Cursor", L"Monitor your Cursor usage."},
            {L"show_grok", L"show_grok_icon", L"grok_accent", L"Grok", L"grok_available", L"Grok", L"Monitor your Grok usage."},
            {L"show_copilot", L"show_copilot_icon", L"copilot_accent", L"Copilot", L"copilot_available", L"GitHub Copilot", L"Monitor your GitHub Copilot usage."},
        }};
    }

    static std::wstring ProviderUnavailableDetail(ProviderAppearanceEntry const& provider, bool enabled)
    {
        const auto next = NativeLocalization::Format(enabled
            ? L"Turn it off or set up {provider} to resume monitoring."
            : L"Set up {provider} to enable monitoring.", g_nativeLanguage, provider.setupName);
        return NativeLocalization::Format(L"No local setup for {provider} was detected. {next}",
            g_nativeLanguage, provider.setupName, next);
    }

    void MainWindow::UpdateProviderAvailability()
    {
        const auto providers = ProviderAppearanceMeta();
        if (providerSpacingSelector_)
        {
            size_t enabledProviders = 0;
            for (auto const& provider : providers)
                if (AppearanceProviderEnabled(provider.enable)) ++enabledProviders;
            providerSpacingSelector_.IsEnabled(enabledProviders > 1);
        }
        const auto count = std::min(providerToggles_.size(), providers.size());
        for (size_t i = 0; i < count; ++i)
        {
            if (!providerToggles_[i]) continue;
            const bool available = state_ && state_.GetNamedBoolean(hstring(providers[i].availability), false);
            providerToggles_[i].IsEnabled(available || Bool(providers[i].enable));
        }
        if (providerDetailEnableToggle_ && page_ == L"Appearance" && detailProvider_ >= 0 &&
            detailProvider_ < static_cast<int>(providers.size()))
        {
            const auto& provider = providers[static_cast<size_t>(detailProvider_)];
            const bool available = state_ && state_.GetNamedBoolean(hstring(provider.availability), false);
            providerDetailEnableToggle_.IsEnabled(available || Bool(provider.enable));
        }
    }
    void MainWindow::UpdateAboutControls()
    {
        if (versionText_ && page_ == L"About")
        {
            const auto version = state_ ? std::wstring(state_.GetNamedString(L"version", L"unknown").c_str()) : std::wstring(L"…");
            versionText_.Text(L"v" + version);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(versionText_,
                state_ ? NativeLocalization::Translate(L"Version", g_nativeLanguage) + L" " + version : NativeLocalization::Translate(L"Connecting to the widget…", g_nativeLanguage));
        }
        if (updateStatusText_ && page_ == L"Update")
            updateStatusText_.Text(state_ ? NativeLocalization::Translate(std::wstring(state_.GetNamedString(L"update_status", L"Update status unavailable").c_str()), g_nativeLanguage) : NativeLocalization::Translate(L"Checking update status…", g_nativeLanguage));
        UpdateUpdateIndicator();
        if (checkForUpdatesButton_)
        {
            const bool busy = state_ && state_.GetNamedBoolean(L"update_busy", false);
            const bool available = state_ && state_.GetNamedBoolean(L"update_available", false);
            checkForUpdatesButton_.IsEnabled(!busy);
            checkForUpdatesButton_.Content(box_value(NativeLocalization::Translate(busy ? L"Working…" : available ? L"Install update" : L"Check for updates", g_nativeLanguage)));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(checkForUpdatesButton_,
                NativeLocalization::Translate(busy ? L"Update in progress" : available ? L"Install available update" : L"Check for updates", g_nativeLanguage));
        }
        if (uninstallDescription_)
        {
            const auto channel = state_ ? state_.GetNamedString(L"install_channel", L"portable") : hstring(L"portable");
            uninstallDescription_.Text(NativeLocalization::Translate(channel == L"winget"
                ? L"WinGet will open its uninstall flow and ask you to confirm."
                : L"The built-in flow will ask you to confirm before removing anything.", g_nativeLanguage));
        }
    }
    void MainWindow::UpdateUpdateIndicator()
    {
        const bool available = state_ && state_.GetNamedBoolean(L"update_available", false);
        if (updateIndicator_)
        {
            updateIndicator_.Visibility(available ? Visibility::Visible : Visibility::Collapsed);
            updateIndicator_.HorizontalAlignment(HorizontalAlignment::Right);
            updateIndicator_.VerticalAlignment(VerticalAlignment::Center);
            updateIndicator_.Margin(Navigation().IsPaneOpen()
                ? Thickness{ 0, 0, 12, 0 } : Thickness{ 0, 7, 6, 0 });
        }
        NavigationUpdate().Visibility(available ? Visibility::Visible : Visibility::Collapsed);
        if (!available && page_ == L"Update")
        {
            for (auto const& item : Navigation().FooterMenuItems())
            {
                auto navigationItem = item.try_as<NavigationViewItem>();
                if (navigationItem && std::wstring(unbox_value<hstring>(navigationItem.Tag()).c_str()) == L"About")
                {
                    Navigation().SelectedItem(item);
                    break;
                }
            }
        }
    }
    void MainWindow::UpdateTitleBarLayout()
    {
        if (!hwnd_) return;
        const auto dpi = GetDpiForWindow(hwnd_);
        if (!dpi) return;
        const auto scale = static_cast<double>(dpi) / 96.0;
        const auto titleBar = AppWindow().TitleBar();
        const auto nativeHeight = titleBar.Height();
        // AppWindow title-bar metrics are physical pixels; XAML layout uses DIPs.
        const auto heightDip = nativeHeight > 0 ? static_cast<double>(nativeHeight) / scale : 32.0;
        TitleBarRow().Height(GridLengthHelper::FromValueAndType(heightDip, GridUnitType::Pixel));
        TitleBarDragRegion().Height(heightDip);
        PageContent().Margin(Thickness{ 0, heightDip, 0, 0 });
        TitleBarDragRegion().Margin(Thickness{
            static_cast<double>(titleBar.LeftInset()) / scale,
            0,
            static_cast<double>(titleBar.RightInset()) / scale,
            0 });
    }
    void MainWindow::OpenThirdPartyNotices()
    {
        if (noticesWindow_)
        {
            noticesWindow_.Activate();
            return;
        }

        auto window = Microsoft::UI::Xaml::Window();
        window.Title(NativeLocalization::Translate(L"Third-party licenses and notices", g_nativeLanguage));
        auto layout = Grid();
        layout.Padding(Thickness{ 28, 24, 28, 28 });
        layout.Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
        auto rows = layout.RowDefinitions();
        rows.Append(RowDefinition());
        rows.Append(RowDefinition());
        rows.GetAt(0).Height(GridLengthHelper::Auto());
        rows.GetAt(1).Height(GridLengthHelper::FromValueAndType(1, GridUnitType::Star));
        auto heading = StackPanel(); heading.Spacing(7);
        auto title = Label(L"Third-party licenses and notices", 23);
        title.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
        heading.Children().Append(title);
        auto intro = Label(NativeLocalization::Translate(L"Third-party licenses and notices, plus technology acknowledgments for Quota Usage.", g_nativeLanguage), 13);
        intro.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        intro.Margin(Thickness{ 0, 0, 0, 18 }); heading.Children().Append(intro);
        layout.Children().Append(heading);
        struct ProjectSpec
        {
            wchar_t const* heading;
            wchar_t const* title;
            wchar_t const* automationId;
            bool technologyAcknowledgment;
        };
        static constexpr std::array<ProjectSpec, 6> projectOrder{{
            { L"CODEZENO/CLAUDE-CODE-USAGE-MONITOR", L"CodeZeno / Claude Code Usage Monitor", L"CodeZenoClaudeCodeUsageMonitor", false },
            { L"UPSTREAM-RAY/CODEX-USAGE-MONITOR", L"Upstream Ray / Codex Usage Monitor", L"UpstreamRayCodexUsageMonitor", false },
            { L"LOBEHUB/LOBE-ICONS", L"LobeHub / Lobe Icons", L"LobeHubLobeIcons", false },
            { L"ALGESTEN/UREQ", L"algesten/ureq", L"AlgestenUreq", false },
            { L"RUST-NATIVE-TLS/RUST-NATIVE-TLS", L"rust-native-tls/rust-native-tls", L"RustNativeTls", false },
            { L"MICROSOFT/WINDOWS-APP-SDK-WINUI-3", L"Microsoft WinUI 3 / Windows App SDK", L"MicrosoftWinUI3", true }
        }};
        struct ProjectNotice
        {
            std::wstring title;
            std::wstring automationId;
            std::wstring url;
            std::wstring licenseLabel{ L"License" };
            std::wstring license;
            std::wstring legalText;
            bool technologyAcknowledgment{};
        };

        const auto notices = ResourceText(IDR_THIRD_PARTY_NOTICES);
        std::vector<ProjectNotice> projects;
        bool parseValid = true;
        size_t firstProjectStart = std::wstring::npos;
        const std::wstring urlPrefix{ L"Project URL: " };
        const std::wstring licensePrefix{ L"License: " };
        const std::wstring sourceLicensePrefix{ L"Source license: " };
        auto findProjectHeading = [&notices](std::wstring const& heading, size_t from)
        {
            auto position = from;
            while ((position = notices.find(heading, position)) != std::wstring::npos)
            {
                const bool atLineStart = position == 0 || notices[position - 1] == L'\n';
                const auto end = position + heading.size();
                const bool atLineEnd = end == notices.size() || notices[end] == L'\n' || notices[end] == L'\r';
                if (atLineStart && atLineEnd) return position;
                ++position;
            }
            return std::wstring::npos;
        };
        for (auto const& spec : projectOrder)
        {
            const std::wstring headingText(spec.heading);
            const auto start = findProjectHeading(headingText, 0);
            if (start == std::wstring::npos) { parseValid = false; break; }
            firstProjectStart = std::min(firstProjectStart, start);
            const auto headingEnd = notices.find(L'\n', start);
            if (headingEnd == std::wstring::npos) { parseValid = false; break; }
            auto contentStart = headingEnd + 1;
            auto sectionEnd = notices.size();
            for (auto const& candidate : projectOrder)
            {
                if (std::wstring(candidate.heading) == headingText) continue;
                const auto nextStart = findProjectHeading(candidate.heading, contentStart);
                if (nextStart != std::wstring::npos) sectionEnd = std::min(sectionEnd, nextStart);
            }

            ProjectNotice project;
            project.title = spec.title;
            project.automationId = spec.automationId;
            project.technologyAcknowledgment = spec.technologyAcknowledgment;
            std::wstring legalText;
            for (auto position = contentStart; position < sectionEnd;)
            {
                const auto newline = notices.find(L'\n', position);
                const auto lineEnd = newline == std::wstring::npos || newline > sectionEnd ? sectionEnd : newline;
                const auto line = notices.substr(position, lineEnd - position);
                auto comparableLine = line;
                if (!comparableLine.empty() && comparableLine.back() == L'\r') comparableLine.pop_back();
                if (comparableLine.rfind(urlPrefix, 0) == 0)
                    project.url = comparableLine.substr(urlPrefix.size());
                else if (comparableLine.rfind(licensePrefix, 0) == 0)
                    project.license = comparableLine.substr(licensePrefix.size());
                else if (comparableLine.rfind(sourceLicensePrefix, 0) == 0)
                {
                    project.licenseLabel = L"Source license";
                    project.license = comparableLine.substr(sourceLicensePrefix.size());
                }
                else if (comparableLine == L"THIRD-PARTY LICENSES AND NOTICES" ||
                    comparableLine == L"TECHNOLOGY ACKNOWLEDGMENTS")
                {
                    // Section labels are rendered as UI headings, not legal text.
                }
                else
                {
                    legalText.append(line);
                    if (lineEnd < sectionEnd) legalText.push_back(L'\n');
                }
                position = lineEnd + (lineEnd < sectionEnd ? 1 : 0);
            }
            while (!legalText.empty() && (legalText.front() == L'\r' || legalText.front() == L'\n')) legalText.erase(legalText.begin());
            while (!legalText.empty() && (legalText.back() == L'\r' || legalText.back() == L'\n')) legalText.pop_back();
            project.legalText = std::move(legalText);
            if (project.url.empty() || project.license.empty() || project.legalText.empty()) { parseValid = false; break; }
            projects.push_back(std::move(project));
        }

        auto noticesContent = StackPanel(); noticesContent.Spacing(16);
        auto addReadOnlyText = [](Panel const& parent, std::wstring const& value, std::wstring const& automationId)
        {
            auto text = TextBox();
            text.IsReadOnly(true);
            text.AcceptsReturn(true);
            text.Text(value);
            text.TextWrapping(TextWrapping::Wrap);
            ScrollViewer::SetVerticalScrollBarVisibility(text, ScrollBarVisibility::Auto);
            ScrollViewer::SetHorizontalScrollBarVisibility(text, ScrollBarVisibility::Disabled);
            text.FontFamily(Media::FontFamily(L"Segoe UI"));
            text.FontSize(13);
            text.MinHeight(120);
            text.Padding(Thickness{ 16, 14, 16, 14 });
            text.Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
            text.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
            text.BorderThickness(Thickness{ 1 });
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(text, NativeLocalization::Translate(L"Third-party license and notice text", g_nativeLanguage));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(text, automationId);
            parent.Children().Append(text);
        };

        auto addNoticeCard = [&addReadOnlyText, &noticesContent](std::wstring const& title, std::wstring const& url,
            std::wstring const& licenseLabel, std::wstring const& license,
            std::wstring const& legalText, std::wstring const& automationId)
        {
            auto card = Card();
            auto content = StackPanel(); content.Spacing(12);
            auto heading = Label(title, 18);
            heading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            content.Children().Append(heading);

            auto metadata = Grid(); metadata.ColumnSpacing(14);
            auto urlColumn = ColumnDefinition(); urlColumn.Width(GridLength{ 1, GridUnitType::Star });
            auto licenseColumn = ColumnDefinition(); licenseColumn.Width(GridLength{ 1, GridUnitType::Auto });
            metadata.ColumnDefinitions().Append(urlColumn); metadata.ColumnDefinitions().Append(licenseColumn);
            auto urlDetails = Grid(); urlDetails.ColumnSpacing(6);
            auto urlLabelColumn = ColumnDefinition(); urlLabelColumn.Width(GridLength{ 1, GridUnitType::Auto });
            auto urlValueColumn = ColumnDefinition(); urlValueColumn.Width(GridLength{ 1, GridUnitType::Star });
            urlDetails.ColumnDefinitions().Append(urlLabelColumn); urlDetails.ColumnDefinitions().Append(urlValueColumn);
            auto urlLabel = Label(NativeLocalization::Translate(L"Project URL", g_nativeLanguage) + L":", 12);
            urlLabel.HorizontalAlignment(HorizontalAlignment::Left);
            urlLabel.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            auto urlText = TextBlock(); urlText.Text(url); urlText.TextWrapping(TextWrapping::Wrap);
            urlText.HorizontalAlignment(HorizontalAlignment::Left);
            urlText.Foreground(Application::Current().Resources().Lookup(box_value(L"AccentTextFillColorPrimaryBrush")).as<Media::Brush>());
            auto urlLink = HyperlinkButton(); urlLink.Content(urlText); urlLink.NavigateUri(Windows::Foundation::Uri(url));
            urlLink.HorizontalAlignment(HorizontalAlignment::Stretch);
            urlLink.HorizontalContentAlignment(HorizontalAlignment::Left);
            urlLink.Padding(Thickness{ 0, 2, 0, 2 });
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(urlLink,
                NativeLocalization::Translate(L"Open project URL", g_nativeLanguage) + L": " + url);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(urlLink, L"ThirdPartyProjectUrl_" + automationId);
            urlLabel.VerticalAlignment(VerticalAlignment::Center);
            urlDetails.Children().Append(urlLabel);
            Grid::SetColumn(urlLink, 1);
            urlDetails.Children().Append(urlLink);
            Grid::SetColumn(urlDetails, 0);
            metadata.Children().Append(urlDetails);

            auto licenseDetails = NativeLocalization::Translate(licenseLabel, g_nativeLanguage) + L": " + license;
            auto licenseText = Label(licenseDetails, 12);
            licenseText.VerticalAlignment(VerticalAlignment::Center);
            Grid::SetColumn(licenseText, 1);
            metadata.Children().Append(licenseText);
            content.Children().Append(metadata);
            addReadOnlyText(content, legalText, L"ThirdPartyNoticeText_" + automationId);
            card.Child(content);
            noticesContent.Children().Append(card);
        };

        if (parseValid && projects.size() == projectOrder.size())
        {
            auto projectLicenseCard = Card();
            auto licenseStack = StackPanel(); licenseStack.Spacing(10);
            auto licenseHeading = Label(NativeLocalization::Translate(L"Quota Usage project license", g_nativeLanguage), 18);
            licenseHeading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            licenseStack.Children().Append(licenseHeading);
            auto projectLicenseText = notices.substr(0, firstProjectStart);
            const auto thirdPartyHeadingOffset = projectLicenseText.find(L"THIRD-PARTY LICENSES AND NOTICES");
            if (thirdPartyHeadingOffset != std::wstring::npos) projectLicenseText.erase(thirdPartyHeadingOffset);
            while (!projectLicenseText.empty() && (projectLicenseText.back() == L'\r' || projectLicenseText.back() == L'\n'))
                projectLicenseText.pop_back();
            addReadOnlyText(licenseStack, projectLicenseText, L"CodexUsageProjectLicenseText");
            projectLicenseCard.Child(licenseStack);
            noticesContent.Children().Append(projectLicenseCard);
            auto thirdPartyHeading = Label(NativeLocalization::Translate(L"Third-party licenses and notices", g_nativeLanguage), 19);
            thirdPartyHeading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            noticesContent.Children().Append(thirdPartyHeading);
            for (auto const& project : projects)
                if (!project.technologyAcknowledgment)
                    addNoticeCard(project.title, project.url, project.licenseLabel, project.license,
                        project.legalText, project.automationId);
            auto technologyHeading = Label(NativeLocalization::Translate(L"Technology acknowledgments", g_nativeLanguage), 19);
            technologyHeading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            noticesContent.Children().Append(technologyHeading);
            for (auto const& project : projects)
                if (project.technologyAcknowledgment)
                    addNoticeCard(project.title, project.url, project.licenseLabel, project.license,
                        project.legalText, project.automationId);
        }
        else
        {
            auto fallback = Card();
            auto fallbackContent = StackPanel(); fallbackContent.Spacing(10);
            fallbackContent.Children().Append(Label(L"Third-party license and notice text", 18));
            addReadOnlyText(fallbackContent, notices, L"ThirdPartyNoticeText");
            fallback.Child(fallbackContent);
            noticesContent.Children().Append(fallback);
        }

        auto noticesScroll = ScrollViewer();
        noticesScroll.VerticalScrollBarVisibility(ScrollBarVisibility::Auto);
        noticesScroll.HorizontalScrollBarVisibility(ScrollBarVisibility::Disabled);
        noticesScroll.Content(noticesContent);
        Grid::SetRow(noticesScroll, 1);
        layout.Children().Append(noticesScroll);
        window.Content(layout);

        const auto ownerDpi = GetDpiForWindow(hwnd_);
        const auto dpi = ownerDpi ? ownerDpi : 96;
        auto width = MulDiv(900, dpi, 96);
        auto height = MulDiv(700, dpi, 96);
        RECT owner{};
        MONITORINFO monitor{ sizeof(monitor) };
        const bool hasOwner = GetWindowRect(hwnd_, &owner) != FALSE;
        if (hasOwner && GetMonitorInfoW(MonitorFromWindow(hwnd_, MONITOR_DEFAULTTONEAREST), &monitor))
        {
            const auto workWidth = monitor.rcWork.right - monitor.rcWork.left;
            const auto workHeight = monitor.rcWork.bottom - monitor.rcWork.top;
            width = std::min<int>(width, static_cast<int>(workWidth));
            height = std::min<int>(height, static_cast<int>(workHeight));
            const auto left = std::clamp<int>(static_cast<int>(owner.left + ((owner.right - owner.left) - width) / 2),
                static_cast<int>(monitor.rcWork.left), static_cast<int>(monitor.rcWork.right - width));
            const auto top = std::clamp<int>(static_cast<int>(owner.top + ((owner.bottom - owner.top) - height) / 2),
                static_cast<int>(monitor.rcWork.top), static_cast<int>(monitor.rcWork.bottom - height));
            window.AppWindow().MoveAndResize({ left, top, width, height });
        }
        else window.AppWindow().Resize({ width, height });
        ApplyWindowIcon(window, windowIcon_);
        noticesWindow_ = window;
        auto weak = get_weak();
        noticesWindow_.Closed([weak](auto const& sender, auto const&)
        {
            if (auto self = weak.get(); self && self->noticesWindow_ == sender.as<Microsoft::UI::Xaml::Window>())
                self->noticesWindow_ = nullptr;
        });
        noticesWindow_.Activate();
    }
    void MainWindow::Hydrate()
    {
        SyncPaneState();
        RenderPage(page_);
        UpdateSaveBar();
    }

    void MainWindow::SyncPaneState()
    {
        const auto settings = State();
        const auto paneOpen = settings.GetNamedBoolean(L"settings_pane_open", true);
        const bool wasHydrating = hydrating_.load();
        hydrating_ = true;
        Navigation().IsPaneOpen(paneOpen);
        hydrating_ = wasHydrating;
        paneStateKnown_ = true;
    }

    void MainWindow::QueuePaneState(bool open)
    {
        const auto current = state_ ? state_.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        const bool authoritativeValue = current.GetNamedBoolean(L"settings_pane_open", true);
        if (pendingPaneState_ && *pendingPaneState_ == open) return;
        if (paneStateRequestInFlight_ && paneStateRequestValue_ && *paneStateRequestValue_ == open)
        {
            pendingPaneState_.reset();
            return;
        }
        if (!pendingPaneState_ && !paneStateRequestInFlight_ && authoritativeValue == open) return;
        pendingPaneState_ = open;
        SendPendingPaneState();
    }

    void MainWindow::SendPendingPaneState()
    {
        if (!pendingPaneState_ || paneStateRequestInFlight_ || inFlight_ != 0 || stopping_ || closed_) return;
        const bool requested = *pendingPaneState_;
        const auto current = state_ ? state_.GetNamedObject(L"settings", JsonObject()) : JsonObject();
        if (current.GetNamedBoolean(L"settings_pane_open", true) == requested)
        {
            pendingPaneState_.reset();
            return;
        }
        auto request = Request(L"set");
        request.Insert(L"key", JsonValue::CreateStringValue(L"settings_pane_open"));
        request.Insert(L"value", JsonValue::CreateBooleanValue(requested));
        pendingPaneState_.reset();
        paneStateRequestValue_ = requested;
        paneStateRequestInFlight_ = true;
        Send(request);
        if (inFlight_ == 0)
        {
            paneStateRequestInFlight_ = false;
            paneStateRequestValue_.reset();
            pendingPaneState_ = requested;
        }
    }

    void MainWindow::SetColorDraft(std::wstring const& key, std::optional<std::wstring> draft)
    {
        if (key == L"claude_accent") claudeColorDraft_ = std::move(draft);
        else if (key == L"codex_accent") codexColorDraft_ = std::move(draft);
        else if (key == L"antigravity_accent") antigravityColorDraft_ = std::move(draft);
        else if (key == L"opencode_accent") opencodeColorDraft_ = std::move(draft);
        else if (key == L"cursor_accent") cursorColorDraft_ = std::move(draft);
        else if (key == L"grok_accent") grokColorDraft_ = std::move(draft);
        else if (key == L"copilot_accent") copilotColorDraft_ = std::move(draft);
        else if (key == L"usage_text_color") usageTextColorDraft_ = std::move(draft);
        appearanceColorDraft_ = claudeColorDraft_.has_value() || codexColorDraft_.has_value() ||
            antigravityColorDraft_.has_value() || opencodeColorDraft_.has_value() || cursorColorDraft_.has_value() ||
            grokColorDraft_.has_value() || copilotColorDraft_.has_value() || usageTextColorDraft_.has_value();
        UpdateSaveBar();
    }

    void MainWindow::CompleteColorApply(std::wstring const& key, std::optional<std::wstring> const& appliedDraft)
    {
        if (!appliedDraft) return;
        if (key == L"claude_accent" && claudeColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"codex_accent" && codexColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"antigravity_accent" && antigravityColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"opencode_accent" && opencodeColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"cursor_accent" && cursorColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"grok_accent" && grokColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"copilot_accent" && copilotColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
        else if (key == L"usage_text_color" && usageTextColorDraft_ == appliedDraft)
            SetColorDraft(key, std::nullopt);
    }

    bool MainWindow::HasUnappliedColorDraft() const
    {
        return (claudeColorDraft_ && claudeColorApplyText_ != claudeColorDraft_ &&
                flightClaudeColorApplyText_ != claudeColorDraft_ && persistClaudeColorApplyText_ != claudeColorDraft_) ||
            (codexColorDraft_ && codexColorApplyText_ != codexColorDraft_ &&
                flightCodexColorApplyText_ != codexColorDraft_ && persistCodexColorApplyText_ != codexColorDraft_) ||
            (antigravityColorDraft_ && antigravityColorApplyText_ != antigravityColorDraft_ &&
                flightAntigravityColorApplyText_ != antigravityColorDraft_ && persistAntigravityColorApplyText_ != antigravityColorDraft_) ||
            (opencodeColorDraft_ && opencodeColorApplyText_ != opencodeColorDraft_ &&
                flightOpencodeColorApplyText_ != opencodeColorDraft_ && persistOpencodeColorApplyText_ != opencodeColorDraft_) ||
            (cursorColorDraft_ && cursorColorApplyText_ != cursorColorDraft_ &&
                flightCursorColorApplyText_ != cursorColorDraft_ && persistCursorColorApplyText_ != cursorColorDraft_) ||
            (grokColorDraft_ && grokColorApplyText_ != grokColorDraft_ &&
                flightGrokColorApplyText_ != grokColorDraft_ && persistGrokColorApplyText_ != grokColorDraft_) ||
            (copilotColorDraft_ && copilotColorApplyText_ != copilotColorDraft_ &&
                flightCopilotColorApplyText_ != copilotColorDraft_ && persistCopilotColorApplyText_ != copilotColorDraft_) ||
            (usageTextColorDraft_ && usageTextColorApplyText_ != usageTextColorDraft_ &&
                flightUsageTextColorApplyText_ != usageTextColorDraft_ && persistUsageTextColorApplyText_ != usageTextColorDraft_);
    }

    void MainWindow::ClearResetColorDraftsIfUnchanged(uint64_t claudeRevision, uint64_t codexRevision, uint64_t antigravityRevision, uint64_t opencodeRevision, uint64_t cursorRevision, uint64_t grokRevision, uint64_t copilotRevision, uint64_t usageTextRevision)
    {
        if (claudeColorDraftRevision_ == claudeRevision)
        {
            SetColorDraft(L"claude_accent", std::nullopt);
            claudeColorApplyText_.reset();
        }
        if (codexColorDraftRevision_ == codexRevision)
        {
            SetColorDraft(L"codex_accent", std::nullopt);
            codexColorApplyText_.reset();
        }
        if (antigravityColorDraftRevision_ == antigravityRevision)
        {
            SetColorDraft(L"antigravity_accent", std::nullopt);
            antigravityColorApplyText_.reset();
        }
        if (opencodeColorDraftRevision_ == opencodeRevision)
        {
            SetColorDraft(L"opencode_accent", std::nullopt);
            opencodeColorApplyText_.reset();
        }
        if (cursorColorDraftRevision_ == cursorRevision)
        {
            SetColorDraft(L"cursor_accent", std::nullopt);
            cursorColorApplyText_.reset();
        }
        if (grokColorDraftRevision_ == grokRevision)
        {
            SetColorDraft(L"grok_accent", std::nullopt);
            grokColorApplyText_.reset();
        }
        if (copilotColorDraftRevision_ == copilotRevision)
        {
            SetColorDraft(L"copilot_accent", std::nullopt);
            copilotColorApplyText_.reset();
        }
        if (usageTextColorDraftRevision_ == usageTextRevision)
        {
            SetColorDraft(L"usage_text_color", std::nullopt);
            usageTextColorApplyText_.reset();
        }
    }

    void MainWindow::RefreshSettingsControlsIfSafe()
    {
        if (!settingsRefreshPending_ || initialGetPending_ || inFlight_ != 0 || mutationInFlight_) return;
        const auto effectiveLanguage = state_ ? std::wstring(state_.GetNamedString(L"effective_language", L"en").c_str()) : std::wstring(L"en");
        const bool languageChanged = effectiveLanguage != g_nativeLanguage;
        if (!languageChanged && (HasPendingChanges() || appearanceColorDraft_)) return;
        // State() overlays pending edits and color drafts live outside the
        // controls, so rebuilding copy preserves both. Never interrupt a live
        // pointer gesture; the drop handler retries the deferred refresh.
        if (languageChanged && (providerDragActive_ || providerPointerPressed_)) return;
        settingsRefreshPending_ = false;
        if (languageChanged)
        {
            RenderPage(page_);
            UpdateSaveBar();
            return;
        }
        Hydrate();
    }

    void MainWindow::OnNavigationChanged(NavigationView const&, NavigationViewSelectionChangedEventArgs const& args)
    {
        auto item = args.SelectedItem().try_as<NavigationViewItem>(); if (!item) return;
        const auto nextPage = std::wstring(unbox_value<hstring>(item.Tag()).c_str());
        if (nextPage == page_) return;
        page_ = nextPage;
        if (pageScroll_) pageScroll_.ScrollToVerticalOffset(0);
        RenderPage(page_);
    }
    void MainWindow::RenderPage(std::wstring const& page)
    {
        CancelProviderReorder();
        page_ = page; PageHost().Children().Clear();
        appearancePresetToggles_.clear();
        appearancePresetCombos_.clear();
        appearancePresetColorInputs_.clear();
        widgetVisibilityToggle_ = nullptr;
        widgetDependentCards_.clear();
        UpdateWidgetPageAvailability();
        providerOrderGrid_ = nullptr;
        providerOrderOverlay_ = nullptr;
        providerInsertionIndicator_ = nullptr;
        providerFullOrder_.clear();
        ++widgetPreviewGeneration_;
        ++widgetPreviewImageGeneration_;
        ++appearancePresetGeneration_;
        appearancePresetPreviewJobs_.clear();
        appearancePresetPreviewNext_ = 0;
        appearancePresetImages_.clear();
        appearancePresetStatuses_.clear();
        displayedWidgetPreviewBase64_.clear();
        if (page != L"Appearance")
        {
            if (previewTimer_) previewTimer_.Stop();
            previewPending_ = false;
            detailProvider_ = -1;
        }
        providerToggles_.clear();
        providerDetailEnableToggle_ = nullptr;
        providerIconPositionSelector_ = nullptr;
        providerSpacingSelector_ = nullptr;
        widgetPreviewImage_ = nullptr;
        widgetPreviewStatus_ = nullptr;
        ClearStickyPreview();
        versionText_ = nullptr;
        updateStatusText_ = nullptr;
        uninstallDescription_ = nullptr;
        checkForUpdatesButton_ = nullptr;
        const auto language = state_ ? std::wstring(state_.GetNamedString(L"effective_language", L"en").c_str()) : std::wstring(L"en");
        g_nativeLanguage = language;
        Title(L"Quota Usage — " + NativeLocalization::Translate(L"Settings", g_nativeLanguage));
        NavigationHeading().Text(NativeLocalization::Translate(L"Settings", g_nativeLanguage));
        SaveHint().Text(NativeLocalization::Translate(L"Changes take effect after you save them.", g_nativeLanguage));
        UnsavedLabel().Text(NativeLocalization::Translate(L"Unsaved changes", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(MessageBar(), NativeLocalization::Translate(L"Settings feedback", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(SaveBar(), NativeLocalization::Translate(L"Unsaved changes", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(TitleBarDragRegion(), NativeLocalization::Translate(L"Window drag area", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(SaveChangesButton(), NativeLocalization::Translate(L"Save changes", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(DiscardChangesButton(), NativeLocalization::Translate(L"Discard changes", g_nativeLanguage));
        ToolTipService::SetToolTip(SaveChangesButton(), box_value(NativeLocalization::Translate(L"Save changes", g_nativeLanguage)));
        ToolTipService::SetToolTip(DiscardChangesButton(), box_value(NativeLocalization::Translate(L"Discard changes", g_nativeLanguage)));
        NavigationGeneralItem().Content(box_value(NativeLocalization::Translate(L"General", g_nativeLanguage)));
        NavigationMonitoringItem().Content(box_value(NativeLocalization::Translate(L"Monitoring", g_nativeLanguage)));
        NavigationAppearanceItem().Content(box_value(NativeLocalization::Translate(L"Appearance", g_nativeLanguage)));
        NavigationPlacementItem().Content(box_value(NativeLocalization::Translate(L"Placement", g_nativeLanguage)));
        NavigationUpdate().Content(box_value(NativeLocalization::Translate(L"Update", g_nativeLanguage)));
        NavigationExperimental().Content(box_value(NativeLocalization::Translate(L"Experimental", g_nativeLanguage)));
        for (auto const& item : Navigation().FooterMenuItems())
        {
            // FooterMenuItems stores IInspectable values; project each item
            // before reading its navigation-only Tag and Content properties.
            if (auto navigationItem = item.try_as<NavigationViewItem>())
            {
                if (navigationItem.Tag() && unbox_value<hstring>(navigationItem.Tag()) == L"About")
                    navigationItem.Content(box_value(NativeLocalization::Translate(L"About", g_nativeLanguage)));
            }
        }
        PageTitle().Text(NativeLocalization::Translate(page == L"Update" ? L"Software update" : page, g_nativeLanguage));
        PageEyebrow().Text(NativeLocalization::Translate(page == L"About" ? L"ABOUT QUOTA USAGE" : page == L"Update" ? L"KEEPING CURRENT" : page == L"Experimental" ? L"TRY WITH CARE" : L"PREFERENCES", g_nativeLanguage));
        const std::map<std::wstring, std::wstring> summaries{
            {L"General",L"Manage startup, taskbar visibility, and language."}, {L"Monitoring",L"Choose providers and refresh options."},
            {L"Appearance",L"Choose the widget’s theme and visual settings."}, {L"Placement",L"Choose the display and taskbar position."},
            {L"About",L"A closer look at the app, how it works, and its project details."},
            {L"Experimental",L"Opt in to experimental limits and extras."},
            {L"Update",L"See the current update status and install the latest available version."}};
        auto summary = summaries.find(page);
        PageSummary().Text(summary != summaries.end() ? NativeLocalization::Translate(summary->second, g_nativeLanguage) : std::wstring());
        hydrating_ = true;
#if QUOTA_DEVELOPMENT_UI
        if (page == L"Development") BuildDevelopment(); else
#endif
        if (page == L"General") BuildGeneral(); else if (page == L"Monitoring") BuildMonitoring(); else if (page == L"Appearance") { if (detailProvider_ >= 0 && detailProvider_ < 7) BuildProviderDetail(detailProvider_); else BuildAppearance(); } else if (page == L"Placement") BuildPlacement(); else if (page == L"Update") BuildUpdate(); else if (page == L"Experimental") BuildExperimental(); else BuildAbout();
        hydrating_ = false;
        UpdateProviderAvailability();
    }

    void MainWindow::UpdateWidgetPageAvailability()
    {
        const bool visible = Bool(L"widget_visible");
        const double opacity = visible ? 1.0 : 0.56;
        NavigationAppearanceItem().Opacity(opacity);
        NavigationPlacementItem().Opacity(opacity);
        for (auto const& card : widgetDependentCards_) card.IsEnabled(visible);
    }

    void MainWindow::AppendWidgetDependentCard(Border const& card)
    {
        auto gate = ContentControl();
        gate.Content(card);
        gate.HorizontalAlignment(HorizontalAlignment::Stretch);
        gate.HorizontalContentAlignment(HorizontalAlignment::Stretch);
        gate.VerticalContentAlignment(VerticalAlignment::Top);
        card.HorizontalAlignment(HorizontalAlignment::Stretch);
        gate.Background(nullptr);
        gate.BorderBrush(nullptr);
        gate.BorderThickness(Thickness{ 0 });
        gate.Padding(Thickness{ 0 });
        gate.IsTabStop(false);
        gate.IsEnabled(Bool(L"widget_visible"));
        widgetDependentCards_.push_back(gate);
        PageHost().Children().Append(gate);
    }

    void MainWindow::AddWidgetHiddenWarning()
    {
        auto card = Card();
        card.HorizontalAlignment(HorizontalAlignment::Stretch);
        auto content = Grid();
        content.ColumnSpacing(12);
        auto messageColumn = ColumnDefinition(); messageColumn.Width(GridLength{ 1, GridUnitType::Star });
        auto linkColumn = ColumnDefinition(); linkColumn.Width(GridLength{ 1, GridUnitType::Auto });
        content.ColumnDefinitions().Append(messageColumn);
        content.ColumnDefinitions().Append(linkColumn);
        auto message = Label(L"Widget display is off. Turn it on to use this page.", 13);
        message.TextWrapping(TextWrapping::Wrap);
        message.VerticalAlignment(VerticalAlignment::Center);
        message.Foreground(Application::Current().Resources().Lookup(box_value(L"SystemFillColorCautionBrush")).as<Media::Brush>());
        Grid::SetColumn(message, 0);
        content.Children().Append(message);
        auto link = HyperlinkButton();
        link.Content(box_value(NativeLocalization::Translate(L"Widget settings", g_nativeLanguage)));
        link.HorizontalAlignment(HorizontalAlignment::Right);
        link.VerticalAlignment(VerticalAlignment::Center);
        link.MaxWidth(160);
        link.Foreground(Application::Current().Resources().Lookup(box_value(L"AccentTextFillColorPrimaryBrush")).as<Media::Brush>());
        Grid::SetColumn(link, 1);
        link.Click([this](auto const&, auto const&)
        {
            NavigateTo(L"General");
            auto weak = get_weak();
            dispatcher_.TryEnqueue([weak]()
            {
                if (auto self = weak.get())
                {
                    if (self->pageScroll_) self->pageScroll_.ScrollToVerticalOffset(0);
                    if (self->widgetVisibilityToggle_) self->widgetVisibilityToggle_.Focus(FocusState::Programmatic);
                }
            });
        });
        content.Children().Append(link);
        card.Child(content);
        PageHost().Children().Append(card);
    }

    void MainWindow::BuildGeneral()
    {
        auto card = Card(); auto stack = StackPanel(); stack.Spacing(18);
        stack.Children().Append(Label(L"General settings", 19));
        auto settings = State();
        auto toggle = [this, &stack](std::wstring key, std::wstring title, std::wstring detail)
        {
            auto copy = StackPanel(); copy.Spacing(3); copy.Children().Append(Label(title, 14));
            auto sub = Label(detail, 12); sub.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>()); copy.Children().Append(sub);
            auto sw = ToggleSwitch();
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw, NativeLocalization::Translate(title, g_nativeLanguage));
            const auto automationId = key == L"show_provider_icon" ? std::wstring(L"Setting_show_provider_icons") : std::wstring(L"Setting_") + key;
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(sw, automationId);
            SetSwitchContent(sw);
            sw.IsOn(key == L"show_provider_icon" ? State().GetNamedBoolean(key, true) : Bool(key));
            if (key == L"widget_visible") widgetVisibilityToggle_ = sw;
            sw.Toggled([this, key](auto const& sender, auto const&) { SetValue(key, JsonValue::CreateBooleanValue(sender.as<ToggleSwitch>().IsOn())); });
            stack.Children().Append(ToggleRow(copy, sw));
        };
        toggle(L"startup", L"Start with Windows", L"Open the widget when you sign in.");
        toggle(L"widget_visible", L"Show the taskbar widget", L"Keep the live usage view visible in your taskbar.");
        toggle(L"show_tray_icon", L"Show the Windows notification-area icon", L"Show or hide Quota Usage’s separate icon beside the Windows clock.");
        toggle(L"show_provider_icon", L"Show provider logos inside the widget", L"Show or hide the small provider logos in the usage widget; this does not affect the Windows notification-area icon.");
        auto languageRow = StackPanel(); languageRow.Spacing(8);
        auto languageLabel = TextBlock(); languageLabel.Text(L"Language"); languageLabel.FontSize(14); languageLabel.TextWrapping(TextWrapping::Wrap);
        languageRow.Children().Append(languageLabel);
        auto languages = ComboBox(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(languages, L"Language"); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(languages, L"LanguageSelector"); languages.MinWidth(230); languages.HorizontalAlignment(HorizontalAlignment::Left);
        if (state_ && state_.HasKey(L"languages")) for (auto const& value : state_.GetNamedArray(L"languages"))
        {
            auto language = value.GetObject();
            const std::wstring code(language.GetNamedString(L"code", L"").c_str());
            const std::wstring nativeName(language.GetNamedString(L"name", language.GetNamedString(L"code", L"")).c_str());
            const wchar_t* englishName = code == L"en" ? L"English" :
                code == L"nl" ? L"Dutch" : code == L"es" ? L"Spanish" :
                code == L"fr" ? L"French" : code == L"de" ? L"German" :
                code == L"ja" ? L"Japanese" : code == L"ko" ? L"Korean" :
                code == L"zh-CN" ? L"Simplified Chinese" :
                code == L"zh-TW" ? L"Traditional Chinese" :
                code == L"ru" ? L"Russian" : code == L"pt-BR" ? L"Brazilian Portuguese" : L"";
            const auto displayName = englishName[0]
                ? nativeName + L" (" + englishName + L")"
                : nativeName + L" (" + code + L")";
            auto item = ComboBoxItem(); item.Content(box_value(displayName)); item.Tag(box_value(code)); languages.Items().Append(item);
        }
        auto system = ComboBoxItem(); system.Content(box_value(L"Use Windows language")); system.Tag(box_value(L"system")); languages.Items().InsertAt(0, system);
        languages.SelectedIndex(0);
        auto selectedLanguage = Text(L"language"); for (uint32_t i=0; i<languages.Items().Size(); ++i) if (unbox_value<hstring>(languages.Items().GetAt(i).as<ComboBoxItem>().Tag()) == selectedLanguage) languages.SelectedIndex(i);
        languages.SelectionChanged([this](auto const& sender, auto const&) { auto control=sender.as<ComboBox>(); if (!hydrating_ && control.SelectedItem()) SetValue(L"language", JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag()))); });
        languageRow.Children().Append(languages); stack.Children().Append(languageRow);
        card.Child(stack); PageHost().Children().Append(card);
        auto exitCard = Card(); auto exitStack = StackPanel(); exitStack.Spacing(8);
        exitStack.Children().Append(Label(L"Close the widget", 16));
        exitStack.Children().Append(Label(L"Quota Usage will stop running until you start it again.", 12));
        auto exit = Button(); exit.Content(box_value(NativeLocalization::Translate(L"Exit Quota Usage", g_nativeLanguage))); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(exit, L"ExitApp"); exit.Click([this](auto const&, auto const&) { Action(L"exit"); }); exitStack.Children().Append(exit);
        exitCard.Child(exitStack); PageHost().Children().Append(exitCard);
    }

    void MainWindow::BuildMonitoring()
    {
        auto providers=Card(); auto p=StackPanel(); p.Spacing(14); p.Children().Append(Label(L"Usage providers",19));
        providerToggles_.clear();
        for (auto const& meta : ProviderAppearanceMeta())
        {
            const bool enabled = Bool(meta.enable);
            const bool available = state_ && state_.GetNamedBoolean(hstring(meta.availability), false);
            auto copy=StackPanel(); copy.Spacing(3); copy.Children().Append(Label(meta.name));
            auto description = Label(available ? meta.monitoringDetail : ProviderUnavailableDetail(meta, enabled), 12);
            description.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            copy.Children().Append(description);
            auto toggle=ToggleSwitch();
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(toggle, meta.name);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(toggle, std::wstring(L"Setting_") + meta.enable);
            SetSwitchContent(toggle); toggle.IsOn(enabled); toggle.IsEnabled(available || enabled);
            const auto key = meta.enable;
            const auto availabilityKey = meta.availability;
            toggle.Toggled([this, key, availabilityKey](auto const& sender, auto const&)
            {
                auto control = sender.as<ToggleSwitch>();
                const bool enabledNow = control.IsOn();
                SetValue(key, JsonValue::CreateBooleanValue(enabledNow));
                const bool availableNow = state_ && state_.GetNamedBoolean(hstring(availabilityKey), false);
                control.IsEnabled(availableNow || enabledNow);
            });
            providerToggles_.push_back(toggle);
            p.Children().Append(ToggleRow(copy, toggle));
        }
        auto note=Label(L"At least one provider must stay enabled.",12);note.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());p.Children().Append(note);
        auto personalizeFooter = StackPanel(); personalizeFooter.Orientation(Orientation::Horizontal); personalizeFooter.Spacing(8);
        auto personalizeNote = Label(L"Want to personalize the appearance?", 12);
        personalizeNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        personalizeNote.VerticalAlignment(VerticalAlignment::Center);
         auto personalizeLink = HyperlinkButton(); personalizeLink.Content(box_value(NativeLocalization::Translate(L"Open Appearance settings", g_nativeLanguage)));
        personalizeLink.Click([this](auto const&, auto const&) { NavigateTo(L"Appearance"); });
        personalizeFooter.Children().Append(personalizeNote); personalizeFooter.Children().Append(personalizeLink);
        p.Children().Append(personalizeFooter); providers.Child(p);PageHost().Children().Append(providers);
        auto widgetCard = Card(); auto widgetSettings = StackPanel(); widgetSettings.Spacing(14);
        widgetSettings.Children().Append(Label(L"Widget contents", 19));
         widgetSettings.Children().Append(Label(L"Choose what appears in the taskbar widget. Keep at least one usage value visible; provider logos alone do not count.", 12));
        auto addWidgetToggle = [this, &widgetSettings](std::wstring const& key, std::wstring const& title, std::wstring const& detail)
        {
            auto copy = StackPanel(); copy.Spacing(3); copy.Children().Append(Label(title));
            auto description = Label(detail, 12);
            description.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            copy.Children().Append(description);
            auto toggle = ToggleSwitch(); SetSwitchContent(toggle);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(toggle, NativeLocalization::Translate(title, g_nativeLanguage));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(toggle, std::wstring(L"Setting_") + key);
            toggle.IsOn(State().GetNamedBoolean(key, true));
            toggle.Toggled([this, key](auto const& sender, auto const&)
            {
                auto control = sender.as<ToggleSwitch>();
                if (hydrating_) return;
                if (!control.IsOn() && key != L"show_provider_icon")
                {
                    auto settings = State();
                    const bool hasOtherValue = (key != L"show_percentage" && settings.GetNamedBoolean(L"show_percentage", true)) ||
                        (key != L"show_bar" && settings.GetNamedBoolean(L"show_bar", true)) ||
                        (key != L"show_reset_time" && settings.GetNamedBoolean(L"show_reset_time", true));
                    if (!hasOtherValue)
                    {
                        const bool wasHydrating = hydrating_.load(); hydrating_ = true;
                        control.IsOn(true); hydrating_ = wasHydrating;
                        ShowMessage(L"Keep at least one of percentage, progress bar, or reset time visible.", true);
                        return;
                    }
                }
                SetValue(key, JsonValue::CreateBooleanValue(control.IsOn()));
            });
            widgetSettings.Children().Append(ToggleRow(copy, toggle));
        };
        addWidgetToggle(L"show_percentage", L"Show usage percentages", L"Show current usage percentages for enabled providers.");
        addWidgetToggle(L"show_bar", L"Show progress bars", L"Show the remaining-usage bars for each provider.");
        addWidgetToggle(L"show_reset_time", L"Show reset times", L"Show when each usage window resets.");
        widgetCard.Child(widgetSettings); PageHost().Children().Append(widgetCard);
        auto options=Card();auto o=StackPanel();o.Spacing(16);o.Children().Append(Label(L"Refresh and alerts",19));
         auto interval=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(interval,NativeLocalization::Translate(L"Polling interval",g_nativeLanguage));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(interval,L"PollingInterval");interval.MinWidth(220);interval.HorizontalAlignment(HorizontalAlignment::Left);
         for(auto [ms,label]:std::array<std::pair<uint32_t,std::wstring>,4>{{{60000,L"Every minute"},{300000,L"Every 5 minutes"},{900000,L"Every 15 minutes"},{3600000,L"Every hour"}}}){auto it=ComboBoxItem();it.Content(box_value(NativeLocalization::Translate(label, g_nativeLanguage)));it.Tag(box_value(ms));interval.Items().Append(it);}
        auto current=State().GetNamedNumber(L"poll_interval_ms",900000);for(uint32_t i=0;i<interval.Items().Size();++i)if(unbox_value<uint32_t>(interval.Items().GetAt(i).as<ComboBoxItem>().Tag())==current)interval.SelectedIndex(i);
        interval.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(L"poll_interval_ms",JsonValue::CreateNumberValue(unbox_value<uint32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));});
        auto intervalLabel=StackPanel();intervalLabel.Spacing(7);intervalLabel.Children().Append(Label(L"Polling interval"));intervalLabel.Children().Append(interval);o.Children().Append(intervalLabel);
         auto adaptiveCopy=StackPanel();adaptiveCopy.Spacing(3);adaptiveCopy.Children().Append(Label(L"Adaptive refresh"));auto adaptiveDetail=Label(L"Adjust refresh timing to match recent usage activity.",12);adaptiveDetail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());adaptiveCopy.Children().Append(adaptiveDetail);
          auto adaptive=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(adaptive,NativeLocalization::Translate(L"Adaptive refresh", g_nativeLanguage));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(adaptive,L"Setting_adaptive_refresh");SetSwitchContent(adaptive);adaptive.IsOn(Bool(L"adaptive_refresh"));adaptive.Toggled([this](auto const&s,auto const&){SetValue(L"adaptive_refresh",JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));});o.Children().Append(ToggleRow(adaptiveCopy,adaptive));
        auto wins=StackPanel();wins.Spacing(8);wins.Children().Append(Label(L"Usage windows"));
          for(auto [key,title]:std::array<std::pair<std::wstring,std::wstring>,2>{{{L"show_session_window",L"Session"},{L"show_weekly_window",L"Weekly"}}}){auto copy=StackPanel();copy.Spacing(3);copy.Children().Append(Label(title));auto detail=Label(title==L"Session"?L"Show the current session usage window.":L"Show the weekly usage window.",12);detail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());copy.Children().Append(detail);auto sw=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw,NativeLocalization::Translate(title, g_nativeLanguage));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(sw,std::wstring(L"Setting_")+key);SetSwitchContent(sw);sw.IsOn(Bool(key));sw.Toggled([this,key](auto const&s,auto const&){SetValue(key,JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));});wins.Children().Append(ToggleRow(copy,sw));}o.Children().Append(wins);
         auto alerts=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(alerts,NativeLocalization::Translate(L"Low quota alerts", g_nativeLanguage));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(alerts,L"LowQuotaAlertThreshold");alerts.MinWidth(220);alerts.HorizontalAlignment(HorizontalAlignment::Left);for(auto [n,label]:std::array<std::pair<int,std::wstring>,4>{{{0,L"Off"},{10,L"10% remaining"},{20,L"20% remaining"},{30,L"30% remaining"}}}){auto it=ComboBoxItem();it.Content(box_value(NativeLocalization::Translate(label, g_nativeLanguage)));it.Tag(box_value(n));alerts.Items().Append(it);}auto threshold=State().GetNamedNumber(L"alert_threshold_percent",0);for(uint32_t i=0;i<alerts.Items().Size();++i)if(unbox_value<int32_t>(alerts.Items().GetAt(i).as<ComboBoxItem>().Tag())==threshold)alerts.SelectedIndex(i);alerts.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(L"alert_threshold_percent",JsonValue::CreateNumberValue(unbox_value<int32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));});auto ar=StackPanel();ar.Spacing(7);ar.Children().Append(Label(L"Low quota alerts"));ar.Children().Append(alerts);o.Children().Append(ar);
         auto refresh = Button(); refresh.Content(box_value(NativeLocalization::Translate(L"Refresh usage now", g_nativeLanguage))); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(refresh,L"RefreshUsage"); refresh.HorizontalAlignment(HorizontalAlignment::Left); refresh.Click([this](auto const&, auto const&) { Action(L"refresh"); }); o.Children().Append(refresh);
        options.Child(o);PageHost().Children().Append(options);
    }

    void MainWindow::AddAccentColorRow(StackPanel const& parent, std::wstring const& key, std::wstring const& title)
    {
        const auto localizedTitle = NativeLocalization::Translate(title, g_nativeLanguage);
        auto group = StackPanel(); group.Spacing(6); group.Children().Append(Label(title));
        const bool accentEnabled = key == L"usage_text_color" || Bool(L"show_bar");
        auto row = StackPanel(); row.Orientation(Orientation::Horizontal); row.Spacing(8);
        row.SizeChanged([](auto const& sender, auto const&)
        {
            auto panel = sender.as<StackPanel>();
            panel.Orientation(panel.ActualWidth() < 610 ? Orientation::Vertical : Orientation::Horizontal);
        });
        auto input = TextBox();
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(input, localizedTitle);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(input, std::wstring(L"Color_") + key);
        appearancePresetColorInputs_.emplace_back(key, input);
        input.PlaceholderText(NativeLocalization::Translate(L"Default color", g_nativeLanguage)); input.Width(145);
        const auto savedText = StoredColorText(State(), key);
        const auto& draft = key == L"claude_accent" ? claudeColorDraft_ : key == L"codex_accent" ? codexColorDraft_ : key == L"antigravity_accent" ? antigravityColorDraft_ : key == L"opencode_accent" ? opencodeColorDraft_ : key == L"cursor_accent" ? cursorColorDraft_ : key == L"grok_accent" ? grokColorDraft_ : key == L"copilot_accent" ? copilotColorDraft_ : usageTextColorDraft_;
        input.Text(draft ? hstring(*draft) : hstring(savedText));
        input.TextChanging([this, key](auto const& sender, auto const&)
        {
            if (hydrating_ || syncingColorText_) return;
            const auto text = std::wstring(sender.as<TextBox>().Text().c_str());
            if (key == L"claude_accent") ++claudeColorDraftRevision_;
            else if (key == L"codex_accent") ++codexColorDraftRevision_;
            else if (key == L"antigravity_accent") ++antigravityColorDraftRevision_;
            else if (key == L"opencode_accent") ++opencodeColorDraftRevision_;
            else if (key == L"cursor_accent") ++cursorColorDraftRevision_;
            else if (key == L"grok_accent") ++grokColorDraftRevision_;
            else if (key == L"copilot_accent") ++copilotColorDraftRevision_;
            else ++usageTextColorDraftRevision_;
            if (key == L"claude_accent" && claudeColorApplyText_ && *claudeColorApplyText_ != text) claudeColorApplyText_.reset();
            if (key == L"codex_accent" && codexColorApplyText_ && *codexColorApplyText_ != text) codexColorApplyText_.reset();
            if (key == L"antigravity_accent" && antigravityColorApplyText_ && *antigravityColorApplyText_ != text) antigravityColorApplyText_.reset();
            if (key == L"opencode_accent" && opencodeColorApplyText_ && *opencodeColorApplyText_ != text) opencodeColorApplyText_.reset();
            if (key == L"cursor_accent" && cursorColorApplyText_ && *cursorColorApplyText_ != text) cursorColorApplyText_.reset();
            if (key == L"grok_accent" && grokColorApplyText_ && *grokColorApplyText_ != text) grokColorApplyText_.reset();
            if (key == L"copilot_accent" && copilotColorApplyText_ && *copilotColorApplyText_ != text) copilotColorApplyText_.reset();
            if (key == L"usage_text_color" && usageTextColorApplyText_ && *usageTextColorApplyText_ != text) usageTextColorApplyText_.reset();
            SetColorDraft(key, text);
            if (key == L"usage_text_color" && ParseHexColor(text))
                SetValue(L"provider_monochrome", JsonValue::CreateBooleanValue(false));
            ScheduleWidgetPreview();
        });

        auto pickerButton = Button(); pickerButton.Content(box_value(NativeLocalization::Translate(L"Choose color…", g_nativeLanguage)));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(pickerButton, std::wstring(L"ChooseColor_") + key);
        auto flyout = Flyout(); auto pickerPanel = StackPanel(); pickerPanel.Spacing(10); pickerPanel.Padding(Thickness{12,12,12,12});
        auto picker = ColorPicker(); picker.IsAlphaEnabled(false); picker.IsColorChannelTextInputVisible(true);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(picker, NativeLocalization::Format(L"%s RGB color", g_nativeLanguage, localizedTitle));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(picker, std::wstring(L"ColorPicker_") + key);
        pickerPanel.Children().Append(picker);
        auto pickerActions = StackPanel(); pickerActions.Orientation(Orientation::Horizontal); pickerActions.Spacing(8);
        auto useColor = Button(); useColor.Content(box_value(NativeLocalization::Translate(L"Use color", g_nativeLanguage)));
        auto cancelColor = Button(); cancelColor.Content(box_value(NativeLocalization::Translate(L"Cancel", g_nativeLanguage)));
        pickerActions.Children().Append(useColor); pickerActions.Children().Append(cancelColor); pickerPanel.Children().Append(pickerActions);
        flyout.Content(pickerPanel); pickerButton.Flyout(flyout);
        flyout.Opened([this, picker, input, key](auto const&, auto const&)
        {
            auto color = ParseHexColor(std::wstring(input.Text().c_str()));
            if (!color) color = ParseHexColor(StoredColorText(State(), key));
            const auto effective = state_ ? state_.GetNamedObject(L"effective_appearance", JsonObject()) : JsonObject();
            const bool systemDark = effective.GetNamedBoolean(L"dark", true);
            const auto palette = Text(L"palette");
            const bool dark = palette == L"high_contrast_dark" || (palette == L"system" && systemDark);
            if (!color && palette == L"system") color = ParseHexColor(EffectiveColorText(state_, key));
            if (!color) color = ParseHexColor(key == L"claude_accent" ? L"#D97757" : key == L"codex_accent"
                ? (dark ? L"#34D399" : L"#0F8F70")
                : key == L"antigravity_accent" ? L"#4285F4" : key == L"opencode_accent" ? L"#5B8DEF"
                : key == L"cursor_accent" ? L"#6E6E6E" : key == L"grok_accent" ? L"#A8A8A8"
                : key == L"copilot_accent" ? L"#A371F7"
                : palette == L"system" ? (dark ? L"#888888" : L"#404040") : (dark ? L"#FFFFFF" : L"#101820"));
            const auto rgb = color.value_or(std::array<uint8_t, 3>{217,119,87});
            picker.Color(Windows::UI::Color{255, rgb[0], rgb[1], rgb[2]});
        });
        useColor.Click([this, picker, input, key, flyout](auto const&, auto const&)
        {
            const auto color = picker.Color();
            const auto value = FormatHexColor({color.R, color.G, color.B});
            syncingColorText_ = true; input.Text(value); syncingColorText_ = false;
            if (key == L"claude_accent") ++claudeColorDraftRevision_;
            else if (key == L"codex_accent") ++codexColorDraftRevision_;
            else if (key == L"antigravity_accent") ++antigravityColorDraftRevision_;
            else if (key == L"opencode_accent") ++opencodeColorDraftRevision_;
            else if (key == L"cursor_accent") ++cursorColorDraftRevision_;
            else if (key == L"grok_accent") ++grokColorDraftRevision_;
            else if (key == L"copilot_accent") ++copilotColorDraftRevision_;
            else ++usageTextColorDraftRevision_;
            SetColorDraft(key, value);
            if (key == L"usage_text_color") SetValue(L"provider_monochrome", JsonValue::CreateBooleanValue(false));
            ScheduleWidgetPreview(); flyout.Hide();
        });
        cancelColor.Click([flyout](auto const&, auto const&) { flyout.Hide(); });

        auto apply = Button(); apply.Content(box_value(NativeLocalization::Translate(L"Apply color", g_nativeLanguage)));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(apply, std::wstring(L"ApplyColor_") + key);
        apply.Click([this, input, key](auto const&, auto const&)
        {
            const std::wstring raw(input.Text().c_str()); const auto color = ParseHexColor(raw);
            if (!color) { SetColorDraft(key, raw); ShowMessage(L"Enter exactly # followed by six ASCII hexadecimal digits, for example #D97757.", true); return; }
            const auto normalized = FormatHexColor(*color);
            syncingColorText_ = true; input.Text(normalized); syncingColorText_ = false;
            SetColorDraft(key, normalized);
            if (key == L"usage_text_color") SetValue(L"provider_monochrome", JsonValue::CreateBooleanValue(false));
            if (key == L"claude_accent") claudeColorApplyText_ = normalized;
            else if (key == L"codex_accent") codexColorApplyText_ = normalized;
            else if (key == L"antigravity_accent") antigravityColorApplyText_ = normalized;
            else if (key == L"opencode_accent") opencodeColorApplyText_ = normalized;
            else if (key == L"cursor_accent") cursorColorApplyText_ = normalized;
            else if (key == L"grok_accent") grokColorApplyText_ = normalized;
            else if (key == L"copilot_accent") copilotColorApplyText_ = normalized;
            else usageTextColorApplyText_ = normalized;
            auto value = JsonArray(); for (const auto component : *color) value.Append(JsonValue::CreateNumberValue(component)); SetValue(key, value);
        });
        auto discard = Button(); discard.Content(box_value(NativeLocalization::Translate(L"Discard", g_nativeLanguage)));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(discard, std::wstring(L"DiscardColor_") + key);
        discard.Click([this, input, key](auto const&, auto const&)
        {
            SetColorDraft(key, std::nullopt);
            if (pendingChanges_ && pendingChanges_.HasKey(key)) { pendingChanges_.Remove(key); if (pendingChanges_.Size() == 0) pendingChanges_ = nullptr; }
            if (key == L"claude_accent") claudeColorApplyText_.reset();
            else if (key == L"codex_accent") codexColorApplyText_.reset();
            else if (key == L"antigravity_accent") antigravityColorApplyText_.reset();
            else if (key == L"opencode_accent") opencodeColorApplyText_.reset();
            else if (key == L"cursor_accent") cursorColorApplyText_.reset();
            else if (key == L"grok_accent") grokColorApplyText_.reset();
            else if (key == L"copilot_accent") copilotColorApplyText_.reset();
            else usageTextColorApplyText_.reset();
            const bool wasHydrating = hydrating_.load(); hydrating_ = true;
            syncingColorText_ = true; input.Text(StoredColorText(State(), key)); syncingColorText_ = false;
            hydrating_ = wasHydrating; ScheduleWidgetPreview(); ShowMessage(L"Color draft discarded."); RefreshSettingsControlsIfSafe();
        });
        input.IsEnabled(accentEnabled); pickerButton.IsEnabled(accentEnabled);
        apply.IsEnabled(accentEnabled); discard.IsEnabled(accentEnabled);
        if (key == L"usage_text_color")
        {
            auto monochrome = Button(); monochrome.Content(box_value(NativeLocalization::Translate(L"Monochrome", g_nativeLanguage)));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(monochrome, L"AppearanceColor_Monochrome");
            monochrome.Click([this, input](auto const&, auto const&)
            {
                SetColorDraft(L"usage_text_color", std::nullopt);
                usageTextColorApplyText_.reset();
                syncingColorText_ = true; input.Text(L""); syncingColorText_ = false;
                SetValue(L"usage_text_color", JsonValue::CreateNullValue());
                SetValue(L"provider_monochrome", JsonValue::CreateBooleanValue(true));
            });
            auto useDefault = Button(); useDefault.Content(box_value(NativeLocalization::Translate(L"Colorful", g_nativeLanguage)));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(useDefault, L"AppearanceColor_Default");
            useDefault.Click([this, input](auto const&, auto const&)
            {
                SetColorDraft(L"usage_text_color", std::nullopt);
                usageTextColorApplyText_.reset();
                syncingColorText_ = true; input.Text(L""); syncingColorText_ = false;
                SetValue(L"usage_text_color", JsonValue::CreateNullValue());
                SetValue(L"provider_monochrome", JsonValue::CreateBooleanValue(false));
            });
            monochrome.IsEnabled(accentEnabled); useDefault.IsEnabled(accentEnabled);
            row.Children().Append(monochrome); row.Children().Append(useDefault);
        }
        group.Children().Append(row); parent.Children().Append(group);
    }

    void MainWindow::UpdatePreviewOverflow()
    {
        if (!previewOverflowWarning_ || !widgetPreviewImage_ || !appearancePreviewCard_) return;
        const auto stack = appearancePreviewCard_.Child().try_as<StackPanel>();
        const double scale = widgetPreviewDpi_ ? static_cast<double>(widgetPreviewDpi_) / 96.0 : 1.0;
        const double previewWidth = widgetPreviewWidthPx_ ? widgetPreviewWidthPx_ / scale : widgetPreviewImage_.ActualWidth();
        const bool tooWide = stack && widgetPreviewImage_.Visibility() == Visibility::Visible &&
            previewWidth > 0 && stack.ActualWidth() > 0 && previewWidth > stack.ActualWidth();
        const bool show = tooWide && Bool(L"allow_unlimited_providers");
        previewOverflowWarning_.Visibility(show ? Visibility::Visible : Visibility::Collapsed);
    }

    void MainWindow::BuildPreviewCard()
    {
        auto previewCard = Card();
        auto previewStack = StackPanel(); previewStack.Spacing(10);
        auto previewHeading = Label(L"Live widget preview · sample values", 17);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(previewHeading, L"AppearancePreview");
        previewStack.Children().Append(previewHeading);
        auto previewNote = Label(L"This image is rendered by the widget using sample usage data.", 12);
        previewNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(previewNote, L"AppearancePreviewSummary");
        previewStack.Children().Append(previewNote);
        widgetPreviewImage_ = Image();
        widgetPreviewImage_.Stretch(Media::Stretch::Uniform);
        widgetPreviewImage_.HorizontalAlignment(HorizontalAlignment::Left);
         Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(widgetPreviewImage_, NativeLocalization::Translate(L"Widget appearance preview with sample usage values", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(widgetPreviewImage_, L"WidgetPreviewImage");
        widgetPreviewImage_.Visibility(Visibility::Collapsed);
        previewStack.Children().Append(widgetPreviewImage_);
        widgetPreviewStatus_ = Label(L"Waiting for the widget’s sample preview…", 12);
        widgetPreviewStatus_.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(widgetPreviewStatus_, L"WidgetPreviewStatus");
        previewStack.Children().Append(widgetPreviewStatus_);
        previewOverflowWarning_ = Label(L"The full preview needs a bigger window to fit. Widen the Settings window to see every provider.", 12);
        previewOverflowWarning_.Foreground(Application::Current().Resources().Lookup(box_value(L"SystemFillColorCautionBrush")).as<Media::Brush>());
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(previewOverflowWarning_, L"PreviewOverflowWarning");
        previewOverflowWarning_.Visibility(Visibility::Collapsed);
        previewOverflowWarning_.TextWrapping(TextWrapping::Wrap);
        previewStack.Children().Append(previewOverflowWarning_);
        previewStack.SizeChanged([this](auto const&, auto const&) { UpdatePreviewOverflow(); });
        previewCard.Child(previewStack); PageHost().Children().Append(previewCard);
        appearancePreviewCard_ = previewCard;
        ScheduleWidgetPreview();
        UpdateWidgetPreview(state_);
    }

    void MainWindow::BeginProviderReorder(Border const& tile, Input::PointerRoutedEventArgs const& args)
    {
        if (!providerOrderGrid_ || !providerOrderOverlay_ || !tile.Tag()) return;
        if (!args.GetCurrentPoint(tile).Properties().IsLeftButtonPressed()) return;
        CancelProviderReorder();
        uint32_t itemIndex{};
        if (!providerOrderGrid_.Items().IndexOf(tile, itemIndex)) return;
        if (!providerOrderGrid_.CapturePointer(args.Pointer())) return;

        providerDraggedTile_ = tile;
        providerDragOriginalIndex_ = static_cast<int>(itemIndex);
        providerDragProviderIndex_ = -1;
        const auto id = std::wstring(unbox_value<hstring>(tile.Tag()).c_str());
        const std::array<std::wstring, 7> ids{ L"claude", L"codex", L"antigravity", L"opencode", L"cursor", L"grok", L"copilot" };
        const auto found = std::find(ids.begin(), ids.end(), id);
        if (found != ids.end()) providerDragProviderIndex_ = static_cast<int>(std::distance(ids.begin(), found));
        providerPointer_ = args.Pointer();
        providerPointerId_ = args.Pointer().PointerId();
        providerPressPoint_ = args.GetCurrentPoint(providerOrderOverlay_).Position();
        providerPointerPressed_ = true;
        providerDragActive_ = false;
        providerDropIndex_.reset();
        args.Handled(true);
    }

    void MainWindow::MoveProviderReorder(Input::PointerRoutedEventArgs const& args)
    {
        if (!providerPointerPressed_ || !providerOrderGrid_ || args.Pointer().PointerId() != providerPointerId_) return;
        const auto point = args.GetCurrentPoint(providerOrderOverlay_).Position();
        if (!providerDragActive_)
        {
            const auto dpi = GetDpiForWindow(hwnd_);
            const auto dragThresholdPx = std::max(GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG));
            const auto dragThresholdDip = std::max(4.0, dpi ? static_cast<double>(dragThresholdPx) * 96.0 / dpi : static_cast<double>(dragThresholdPx));
            if (std::max(std::abs(point.X - providerPressPoint_.X), std::abs(point.Y - providerPressPoint_.Y)) < dragThresholdDip) return;

            providerDragActive_ = true;
            providerOrderGrid_.Items().RemoveAt(static_cast<uint32_t>(*providerDragOriginalIndex_));
            providerOrderOverlay_.Children().Append(providerDraggedTile_);
            Canvas::SetZIndex(providerDraggedTile_, 1);
            Canvas::SetZIndex(providerInsertionIndicator_, 2);
            providerDraggedTile_.IsHitTestVisible(false);
            providerDraggedTile_.Opacity(0.82);
        }
        Canvas::SetLeft(providerDraggedTile_, point.X - providerDraggedTile_.ActualWidth() / 2.0);
        Canvas::SetTop(providerDraggedTile_, point.Y - providerDraggedTile_.ActualHeight() / 2.0);
        providerOrderGrid_.UpdateLayout();
        UpdateProviderDropIndicator(point);
        args.Handled(true);
    }

    void MainWindow::UpdateProviderDropIndicator(Windows::Foundation::Point const& point)
    {
        if (!providerDragActive_ || !providerInsertionIndicator_ || !providerOrderGrid_ || !providerOrderOverlay_)
            return;
        const auto width = providerOrderOverlay_.ActualWidth();
        const auto height = providerOrderOverlay_.ActualHeight();
        if (point.X < 0 || point.Y < 0 || point.X > width || point.Y > height)
        {
            providerDropIndex_.reset();
            providerInsertionIndicator_.Visibility(Visibility::Collapsed);
            return;
        }

        const auto count = providerOrderGrid_.Items().Size();
        if (count == 0)
        {
            providerDropIndex_ = 0;
            Canvas::SetLeft(providerInsertionIndicator_, 2.0);
            Canvas::SetTop(providerInsertionIndicator_, std::max(0.0, (height - providerInsertionIndicator_.Height()) / 2.0));
            providerInsertionIndicator_.Visibility(Visibility::Visible);
            return;
        }

        double bestDistance = std::numeric_limits<double>::max();
        double bestLeft = 0;
        double bestTop = 0;
        int bestIndex = 0;
        // Use realized item bounds in the wrapped layout. The nearest slot's
        // index is exactly the index inserted on release, so the cue and result
        // stay in sync at every responsive column count.
        for (uint32_t index = 0; index <= count; ++index)
        {
            auto container = providerOrderGrid_.ContainerFromIndex(static_cast<int32_t>(std::min(index, count - 1))).try_as<FrameworkElement>();
            if (!container) continue;
            const auto origin = container.TransformToVisual(providerOrderOverlay_).TransformPoint({ 0, 0 });
            const bool afterLast = index == count;
            const double left = std::clamp(
                origin.X + (afterLast ? container.ActualWidth() + 3.0 : -3.0),
                0.0,
                std::max(0.0, width - providerInsertionIndicator_.Width()));
            const double top = origin.Y + (container.ActualHeight() - providerInsertionIndicator_.Height()) / 2.0;
            const double centerY = top + providerInsertionIndicator_.Height() / 2.0;
            const double dx = point.X - left;
            const double dy = point.Y - centerY;
            const double distance = dx * dx + dy * dy;
            if (distance < bestDistance)
            {
                bestDistance = distance;
                bestLeft = left;
                bestTop = top;
                bestIndex = static_cast<int>(index);
            }
        }
        providerDropIndex_ = bestIndex;
        Canvas::SetLeft(providerInsertionIndicator_, bestLeft);
        Canvas::SetTop(providerInsertionIndicator_, bestTop);
        providerInsertionIndicator_.Visibility(Visibility::Visible);
    }

    void MainWindow::EndProviderReorder(Input::PointerRoutedEventArgs const& args)
    {
        if (!providerPointerPressed_ || args.Pointer().PointerId() != providerPointerId_) return;
        const auto point = args.GetCurrentPoint(providerOrderOverlay_).Position();
        const int clickedProvider = providerDragProviderIndex_;
        const bool wasDrag = providerDragActive_;
        if (providerDragActive_)
        {
            UpdateProviderDropIndicator(point);
            const bool canDrop = providerDropIndex_.has_value();
            const int index = canDrop ? *providerDropIndex_ : providerDragOriginalIndex_.value_or(0);
            uint32_t tileIndex{};
            if (providerOrderOverlay_.Children().IndexOf(providerDraggedTile_, tileIndex))
                providerOrderOverlay_.Children().RemoveAt(tileIndex);
            providerDraggedTile_.IsHitTestVisible(true);
            providerDraggedTile_.Opacity(1.0);
            const auto insertionIndex = std::clamp(index, 0, static_cast<int>(providerOrderGrid_.Items().Size()));
            providerOrderGrid_.Items().InsertAt(static_cast<uint32_t>(insertionIndex), providerDraggedTile_);
            if (canDrop) PersistProviderOrder();
            providerDragActive_ = false;
            providerPointerPressed_ = false;
        }
        CancelProviderReorder();
        args.Handled(true);
        if (!wasDrag && clickedProvider >= 0)
        {
            detailProvider_ = clickedProvider;
            RenderPage(L"Appearance");
            if (pageScroll_) pageScroll_.ScrollToVerticalOffset(0);
        }
        RefreshSettingsControlsIfSafe();
    }

    void MainWindow::CancelProviderReorder()
    {
        const auto pointer = providerPointer_;
        const bool restoreTile = providerDragActive_ && providerDraggedTile_ && providerOrderGrid_;
        const int restoreIndex = providerDragOriginalIndex_.value_or(0);
        providerPointerPressed_ = false;
        providerDragActive_ = false;
        providerDragProviderIndex_ = -1;
        providerPointerId_ = 0;
        providerDropIndex_.reset();
        providerDragOriginalIndex_.reset();
        providerPointer_ = nullptr;
        if (providerInsertionIndicator_) providerInsertionIndicator_.Visibility(Visibility::Collapsed);
        if (providerDraggedTile_)
        {
            uint32_t childIndex{};
            if (providerOrderOverlay_ && providerOrderOverlay_.Children().IndexOf(providerDraggedTile_, childIndex))
                providerOrderOverlay_.Children().RemoveAt(childIndex);
            providerDraggedTile_.IsHitTestVisible(true);
            providerDraggedTile_.Opacity(1.0);
            if (restoreTile)
            {
                const auto insertionIndex = std::clamp(restoreIndex, 0, static_cast<int>(providerOrderGrid_.Items().Size()));
                providerOrderGrid_.Items().InsertAt(static_cast<uint32_t>(insertionIndex), providerDraggedTile_);
            }
            providerDraggedTile_ = nullptr;
        }
        if (pointer && providerOrderGrid_) providerOrderGrid_.ReleasePointerCapture(pointer);
    }

    void MainWindow::PersistProviderOrder()
    {
        if (!providerOrderGrid_ || providerFullOrder_.size() != 7) return;
        std::vector<std::wstring> enabledOrder;
        for (auto const& item : providerOrderGrid_.Items())
        {
            auto tile = item.try_as<Border>();
            if (tile && tile.Tag()) enabledOrder.emplace_back(unbox_value<hstring>(tile.Tag()).c_str());
        }
        const auto providers = ProviderAppearanceMeta();
        const std::array<std::wstring, 7> ids{ L"claude", L"codex", L"antigravity", L"opencode", L"cursor", L"grok", L"copilot" };
        auto fullOrder = JsonArray();
        size_t enabledIndex = 0;
        for (const auto& oldId : providerFullOrder_)
        {
            const auto found = std::find(ids.begin(), ids.end(), oldId);
            if (found == ids.end()) continue;
            const auto metaIndex = static_cast<size_t>(std::distance(ids.begin(), found));
            const bool enabled = AppearanceProviderEnabled(providers[metaIndex].enable);
            const auto id = enabled && enabledIndex < enabledOrder.size() ? enabledOrder[enabledIndex++] : oldId;
            fullOrder.Append(JsonValue::CreateStringValue(id));
        }
        if (fullOrder.Size() != 7) return;
        std::vector<std::wstring> nextFullOrder;
        for (auto const& value : fullOrder) nextFullOrder.emplace_back(value.GetString().c_str());
        if (nextFullOrder != providerFullOrder_)
        {
            providerFullOrder_ = std::move(nextFullOrder);
            SetValue(L"provider_order", fullOrder);
        }
        uint32_t panelNumber = 0;
        for (auto const& item : providerOrderGrid_.Items())
        {
            auto tile = item.try_as<Border>();
            auto content = tile ? tile.Child().try_as<Grid>() : Grid(nullptr);
            if (content && content.Children().Size() > 1)
                content.Children().GetAt(1).as<TextBlock>().Text(std::to_wstring(++panelNumber));
        }
    }

    void MainWindow::BuildAppearancePresets()
    {
        static const std::array<std::wstring, 6> ids{
            L"default", L"colorful", L"monochrome", L"without_brands", L"logos_and_numbers", L"numbers_only" };
        static const std::array<std::wstring, 6> names{
            L"Default", L"Colorful", L"Monochrome", L"Without brands", L"Logos and numbers", L"Numbers only" };
        static const std::array<std::wstring, 6> descriptions{
            L"The captured factory appearance: familiar colors, slim bars, and all usage details visible.",
            L"Provider logos and every progress bar use their own colors.",
            L"Logos and progress bars share one neutral gray.",
            L"Only colored bars—no provider logos, percentages, or reset times.",
            L"Provider logos and percentages, without bars or reset times.",
            L"Percentages only—no logos, bars, or reset times." };
        static const std::array<std::wstring, 7> accents{
            L"claude_accent", L"codex_accent", L"antigravity_accent", L"opencode_accent",
            L"cursor_accent", L"grok_accent", L"copilot_accent" };
        static const std::array<std::wstring, 7> icons{
            L"show_claude_icon", L"show_codex_icon", L"show_antigravity_icon", L"show_opencode_icon",
            L"show_cursor_icon", L"show_grok_icon", L"show_copilot_icon" };
        auto current = State();
        auto presetValues = [current](uint32_t index)
        {
            auto values = JsonObject();
            auto setBool = [&values](std::wstring const& key, bool value) { values.Insert(key, JsonValue::CreateBooleanValue(value)); };
            values.Insert(L"palette", JsonValue::CreateStringValue(L"system"));
            values.Insert(L"bar_style", JsonValue::CreateStringValue(L"continuous"));
            values.Insert(L"bar_thickness", JsonValue::CreateStringValue(L"slim"));
            values.Insert(L"provider_spacing", JsonValue::CreateStringValue(L"normal"));
            values.Insert(L"font_size", JsonValue::CreateStringValue(L"standard"));
            values.Insert(L"window_label_font_size", JsonValue::CreateNumberValue(9));
            for (auto const& accent : accents) values.Insert(accent, JsonValue::CreateNullValue());
            values.Insert(L"usage_text_color", JsonValue::CreateNullValue());
            values.Insert(L"provider_monochrome", JsonValue::CreateBooleanValue(true));
            auto order = JsonArray();
            for (auto const& provider : std::array<std::wstring, 7>{ L"claude", L"codex", L"antigravity", L"copilot", L"cursor", L"grok", L"opencode" })
                order.Append(JsonValue::CreateStringValue(provider));
            values.Insert(L"provider_order", order);
            setBool(L"show_provider_icon", true);
            values.Insert(L"provider_icon_position", JsonValue::CreateStringValue(L"top"));
            setBool(L"show_percentage", true);
            setBool(L"show_bar", true);
            setBool(L"show_reset_time", true);
            for (auto const& icon : icons) setBool(icon, true);
            if (index == 1 || index == 3 || index == 4)
            {
                setBool(L"provider_monochrome", false);
                values.Insert(L"usage_text_color", JsonValue::CreateNullValue());
                for (auto const& accent : accents) values.Insert(accent, JsonValue::CreateNullValue());
            }
            if (index == 2)
            {
                JsonArray gray; gray.Append(JsonValue::CreateNumberValue(160));
                gray.Append(JsonValue::CreateNumberValue(160)); gray.Append(JsonValue::CreateNumberValue(160));
                values.Insert(L"usage_text_color", gray);
                for (auto const& accent : accents) values.Insert(accent, gray);
            }
            if (index == 3)
            {
                setBool(L"show_provider_icon", false);
                for (auto const& icon : icons) setBool(icon, false);
                setBool(L"show_percentage", false); setBool(L"show_bar", true); setBool(L"show_reset_time", false);
            }
            else if (index == 4)
            {
                setBool(L"show_percentage", true); setBool(L"show_bar", false); setBool(L"show_reset_time", false);
            }
            else if (index == 5)
            {
                setBool(L"show_provider_icon", false);
                for (auto const& icon : icons) setBool(icon, false);
                setBool(L"show_percentage", true); setBool(L"show_bar", false); setBool(L"show_reset_time", false);
            }
            // Keep user ordering and fine-grained label/logo placement for the
            // optional display modes; Default remains the fixed factory snapshot.
            if (index != 0)
            {
                if (current.HasKey(L"provider_order")) values.Insert(L"provider_order", current.GetNamedValue(L"provider_order"));
                if (current.HasKey(L"window_label_font_size")) values.Insert(L"window_label_font_size", current.GetNamedValue(L"window_label_font_size"));
                if (current.HasKey(L"provider_icon_position")) values.Insert(L"provider_icon_position", current.GetNamedValue(L"provider_icon_position"));
            }
            return values;
        };

        auto chooserCard = Card();
        auto chooser = StackPanel(); chooser.Spacing(10);
        chooser.Children().Append(Label(L"Appearance presets", 19));
        auto note = Label(L"Choose a starting point. Each preview is rendered by the widget; you can keep editing and save when ready.", 12);
        note.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        chooser.Children().Append(note);
        auto grid = Grid(); grid.ColumnSpacing(12); grid.RowSpacing(12);
        grid.SizeChanged([grid](auto const& sender, auto const&) mutable
        {
            const auto width = sender.as<Grid>().ActualWidth();
            const int columns = width >= 900 ? 3 : width >= 590 ? 2 : 1;
            grid.ColumnDefinitions().Clear(); grid.RowDefinitions().Clear();
            for (int i = 0; i < columns; ++i) grid.ColumnDefinitions().Append(ColumnDefinition());
            const int rows = (6 + columns - 1) / columns;
            for (int i = 0; i < rows; ++i) grid.RowDefinitions().Append(RowDefinition());
            const auto childCount = std::min<uint32_t>(6, grid.Children().Size());
            for (uint32_t i = 0; i < childCount; ++i)
            {
                Grid::SetColumn(grid.Children().GetAt(i).as<FrameworkElement>(), static_cast<int>(i) % columns);
                Grid::SetRow(grid.Children().GetAt(i).as<FrameworkElement>(), static_cast<int>(i) / columns);
            }
        });
        appearancePresetImages_.clear(); appearancePresetStatuses_.clear(); appearancePresetPreviewJobs_.clear();
        appearancePresetPreviewNext_ = 0;
        const auto generation = appearancePresetGeneration_;
        for (uint32_t index = 0; index < ids.size(); ++index)
        {
            auto card = Border();
            card.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
            card.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
            card.BorderThickness(ThicknessHelper::FromUniformLength(1));
            card.CornerRadius(CornerRadiusHelper::FromUniformRadius(10)); card.Padding(Thickness{ 12, 12, 12, 12 });
            auto content = StackPanel(); content.Spacing(8);
            content.Children().Append(Label(names[index], 16));
            auto imageFrame = Border(); imageFrame.Height(104); imageFrame.CornerRadius(CornerRadiusHelper::FromUniformRadius(6));
            imageFrame.Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
            imageFrame.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
            imageFrame.BorderThickness(ThicknessHelper::FromUniformLength(1));
            auto image = Image(); image.Width(280); image.Height(96); image.Stretch(Media::Stretch::Uniform); image.HorizontalAlignment(HorizontalAlignment::Center);
            image.VerticalAlignment(VerticalAlignment::Center); image.Visibility(Visibility::Collapsed);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(image,
                NativeLocalization::Translate(names[index], g_nativeLanguage) + L" — " +
                NativeLocalization::Translate(L"Widget-rendered preview", g_nativeLanguage));
            imageFrame.Child(image); appearancePresetImages_.push_back(image); content.Children().Append(imageFrame);
            auto description = Label(descriptions[index], 12);
            description.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            content.Children().Append(description);
            auto status = Label(L"Waiting for the widget’s sample preview…", 11);
            status.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            appearancePresetStatuses_.push_back(status); content.Children().Append(status);
            auto use = Button(); use.Content(box_value(NativeLocalization::Translate(L"Use preset", g_nativeLanguage)));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(use, L"AppearancePreset_" + ids[index]);
            use.Click([this, presetValues, index, generation](auto const&, auto const&)
            {
                if (generation != appearancePresetGeneration_) return;
                if (HasUnappliedColorDraft())
                {
                    ShowMessage(NativeLocalization::Translate(L"Apply or discard color drafts before using an appearance preset.", g_nativeLanguage), true);
                    return;
                }
                auto target = presetValues(index);
                ApplyAppearancePreset(target);
            });
            content.Children().Append(use); card.Child(content); grid.Children().Append(card);

            auto target = presetValues(index);
            appearancePresetPreviewJobs_.push_back({ JsonObject::Parse(target.Stringify()), index });
        }
        chooser.Children().Append(grid); chooserCard.Child(chooser); AppendWidgetDependentCard(chooserCard);
        SendNextPresetPreviewIfIdle();
    }

    void MainWindow::BuildAppearance()
    {
        if (!Bool(L"widget_visible")) AddWidgetHiddenWarning();
        BuildPreviewCard();

        auto providersCard = Card(); auto providers = StackPanel(); providers.Spacing(10);
        providers.Children().Append(Label(L"Providers", 19));
        auto providersNote = Label(L"Drag panels to reorder them, or select one to edit its appearance.", 12);
        providersNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        providers.Children().Append(providersNote);
        auto providerTiles = GridView();
        providerTiles.HorizontalAlignment(HorizontalAlignment::Stretch);
        providerTiles.SelectionMode(ListViewSelectionMode::None);
        providerTiles.IsItemClickEnabled(true);
        const auto allProviders = ProviderAppearanceMeta();
        const std::array<std::wstring, 7> providerIds{ L"claude", L"codex", L"antigravity", L"opencode", L"cursor", L"grok", L"copilot" };
        std::vector<int> orderedProviders;
        auto addProviderIndex = [&orderedProviders](int index)
        {
            if (std::find(orderedProviders.begin(), orderedProviders.end(), index) == orderedProviders.end())
                orderedProviders.push_back(index);
        };
        const auto currentState = State();
        if (currentState.HasKey(L"provider_order") && currentState.GetNamedValue(L"provider_order").ValueType() == JsonValueType::Array)
        {
            for (auto const& value : currentState.GetNamedArray(L"provider_order"))
            {
                if (value.ValueType() != JsonValueType::String) continue;
                const auto id = std::wstring(value.GetString().c_str());
                const auto found = std::find(providerIds.begin(), providerIds.end(), id);
                if (found != providerIds.end()) addProviderIndex(static_cast<int>(std::distance(providerIds.begin(), found)));
            }
        }
        for (int i = 0; i < static_cast<int>(allProviders.size()); ++i) addProviderIndex(i);
        providerFullOrder_.clear();
        for (const auto providerIndex : orderedProviders)
            providerFullOrder_.push_back(providerIds[static_cast<size_t>(providerIndex)]);
        int visibleProviderNumber = 0;
        for (int orderIndex = 0; orderIndex < static_cast<int>(orderedProviders.size()); ++orderIndex)
        {
            const int metaIndex = orderedProviders[static_cast<size_t>(orderIndex)];
            const auto& meta = allProviders[static_cast<size_t>(metaIndex)];
            if (!AppearanceProviderEnabled(meta.enable)) continue;
            ++visibleProviderNumber;
            auto tileContent = Grid();
            auto nameColumn = ColumnDefinition(); nameColumn.Width(GridLength{ 1, GridUnitType::Star });
            auto numberColumn = ColumnDefinition(); numberColumn.Width(GridLength{ 1, GridUnitType::Auto });
            auto iconColumn = ColumnDefinition(); iconColumn.Width(GridLength{ 1, GridUnitType::Auto });
            tileContent.ColumnDefinitions().Append(nameColumn); tileContent.ColumnDefinitions().Append(numberColumn); tileContent.ColumnDefinitions().Append(iconColumn);
            auto providerName = Label(meta.name); providerName.VerticalAlignment(VerticalAlignment::Center);
            auto number = Label(std::to_wstring(visibleProviderNumber), 12); number.VerticalAlignment(VerticalAlignment::Center);
            number.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            auto editIcon = SymbolIcon(Symbol::Edit); editIcon.VerticalAlignment(VerticalAlignment::Center);
            editIcon.Margin(Thickness{12, 0, 0, 0});
            Grid::SetColumn(number, 1); Grid::SetColumn(editIcon, 2);
            tileContent.Children().Append(providerName); tileContent.Children().Append(number); tileContent.Children().Append(editIcon);
            auto tileCard = Border();
            tileCard.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
            tileCard.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
            tileCard.BorderThickness(ThicknessHelper::FromUniformLength(1));
            tileCard.CornerRadius(CornerRadiusHelper::FromUniformRadius(8));
            tileCard.Padding(Thickness{12, 8, 12, 8}); tileCard.Width(280); tileCard.Height(64);
            tileCard.Child(tileContent);
            tileCard.Tag(box_value(providerIds[static_cast<size_t>(metaIndex)]));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(tileCard, NativeLocalization::Format(L"%s appearance settings", g_nativeLanguage, meta.name));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(tileCard, std::wstring(L"EditProvider_") + meta.enable);
            providerTiles.Items().Append(tileCard);
            tileCard.PointerPressed([this](auto const& sender, auto const& args)
            {
                BeginProviderReorder(sender.as<Border>(), args);
            });
        }
        providerOrderGrid_ = providerTiles;
        auto providerTilesHost = Grid();
        providerTilesHost.Children().Append(providerTiles);
        providerOrderOverlay_ = Canvas();
        providerOrderOverlay_.HorizontalAlignment(HorizontalAlignment::Stretch);
        providerOrderOverlay_.VerticalAlignment(VerticalAlignment::Stretch);
        providerOrderOverlay_.IsHitTestVisible(false);
        providerInsertionIndicator_ = Shapes::Rectangle();
        providerInsertionIndicator_.Width(4);
        providerInsertionIndicator_.Height(38);
        providerInsertionIndicator_.Fill(nullptr);
        providerInsertionIndicator_.Stroke(Application::Current().Resources().Lookup(box_value(L"AccentTextFillColorPrimaryBrush")).as<Media::Brush>());
        providerInsertionIndicator_.StrokeThickness(2);
        auto dashPattern = Media::DoubleCollection(); dashPattern.Append(3); dashPattern.Append(2);
        providerInsertionIndicator_.StrokeDashArray(dashPattern);
        providerInsertionIndicator_.Visibility(Visibility::Collapsed);
        providerOrderOverlay_.Children().Append(providerInsertionIndicator_);
        providerTilesHost.Children().Append(providerOrderOverlay_);
        providerTiles.PointerMoved([this](auto const&, auto const& args) { MoveProviderReorder(args); });
        providerTiles.PointerReleased([this](auto const&, auto const& args) { EndProviderReorder(args); });
        providerTiles.PointerCanceled([this](auto const&, auto const&) { CancelProviderReorder(); });
        providerTiles.PointerCaptureLost([this](auto const&, auto const&) { CancelProviderReorder(); });
        providerTiles.ItemClick([this, providerIds](auto const&, auto const& args)
        {
            auto tile = args.ClickedItem().as<Border>();
            const auto id = std::wstring(unbox_value<hstring>(tile.Tag()).c_str());
            const auto found = std::find(providerIds.begin(), providerIds.end(), id);
            if (found == providerIds.end()) return;
            detailProvider_ = static_cast<int>(std::distance(providerIds.begin(), found));
            RenderPage(L"Appearance");
            if (pageScroll_) pageScroll_.ScrollToVerticalOffset(0);
        });
        providers.Children().Append(providerTilesHost);
        auto providersFooter = StackPanel(); providersFooter.Orientation(Orientation::Horizontal); providersFooter.Spacing(4);
        auto providersFooterNote = Label(L"Only enabled providers can be personalized here.", 12);
        providersFooterNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        providersFooterNote.VerticalAlignment(VerticalAlignment::Center);
         auto providersFooterLink = HyperlinkButton(); providersFooterLink.Content(box_value(NativeLocalization::Translate(L"Choose your providers", g_nativeLanguage)));
        providersFooterLink.Click([this](auto const&, auto const&) { NavigateTo(L"Monitoring"); });
        providersFooter.Children().Append(providersFooterNote); providersFooter.Children().Append(providersFooterLink);
        providers.Children().Append(providersFooter);
        providersCard.Child(providers); AppendWidgetDependentCard(providersCard);

        auto visibilityCard = Card(); auto visibility = StackPanel(); visibility.Spacing(10);
        visibility.Children().Append(Label(L"Widget contents", 19));
        auto addVisibilityToggle = [this, &visibility](std::wstring const& key, std::wstring const& title)
        {
            auto copy = StackPanel(); copy.Children().Append(Label(title));
            auto toggle = ToggleSwitch(); SetSwitchContent(toggle);
            toggle.IsOn(State().GetNamedBoolean(hstring(key), true));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(toggle, NativeLocalization::Translate(title, g_nativeLanguage));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(toggle, L"Appearance_" + key);
            appearancePresetToggles_.emplace_back(key, toggle);
            toggle.Toggled([this, key](auto const& sender, auto const&)
            {
                if (!hydrating_)
                {
                    const bool enabled = sender.as<ToggleSwitch>().IsOn();
                    SetValue(key, JsonValue::CreateBooleanValue(enabled));
                    if (key == L"show_provider_icon" && providerIconPositionSelector_)
                        providerIconPositionSelector_.IsEnabled(enabled);
                }
            });
            visibility.Children().Append(ToggleRow(copy, toggle));
        };
        addVisibilityToggle(L"show_provider_icon", L"Provider logos");
        addVisibilityToggle(L"show_percentage", L"Percentages");
        addVisibilityToggle(L"show_bar", L"Progress bars");
        auto iconPositionGroup = StackPanel(); iconPositionGroup.Spacing(6);
        iconPositionGroup.Children().Append(Label(L"Provider logo position"));
        auto iconPosition = ComboBox();
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(iconPosition, NativeLocalization::Translate(L"Provider logo position", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(iconPosition, L"Appearance_provider_icon_position");
        for (auto const& [value, label] : std::array<std::pair<std::wstring, std::wstring>, 3>{{
            {L"top", L"Top"}, {L"center", L"Center"}, {L"bottom", L"Bottom"}}})
        {
            auto item = ComboBoxItem(); item.Content(box_value(NativeLocalization::Translate(label, g_nativeLanguage))); item.Tag(box_value(value)); iconPosition.Items().Append(item);
        }
        const auto position = Text(L"provider_icon_position");
        const auto selectedPosition = position.empty() ? std::wstring(L"top") : position;
        for (uint32_t i = 0; i < iconPosition.Items().Size(); ++i)
            if (unbox_value<hstring>(iconPosition.Items().GetAt(i).as<ComboBoxItem>().Tag()) == selectedPosition) iconPosition.SelectedIndex(i);
        iconPosition.MinWidth(250); iconPosition.MaxWidth(360); iconPosition.HorizontalAlignment(HorizontalAlignment::Left);
        iconPosition.IsEnabled(Bool(L"show_provider_icon"));
        providerIconPositionSelector_ = iconPosition;
        appearancePresetCombos_.emplace_back(L"provider_icon_position", iconPosition);
        iconPosition.SelectionChanged([this](auto const& sender, auto const&)
        {
            auto control = sender.as<ComboBox>();
            if (!hydrating_ && control.SelectedItem())
                SetValue(L"provider_icon_position", JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag())));
        });
        iconPositionGroup.Children().Append(iconPosition); visibility.Children().Append(iconPositionGroup);
        visibilityCard.Child(visibility); AppendWidgetDependentCard(visibilityCard);

        auto styleCard = Card();
        auto style = StackPanel(); style.Spacing(16);
        style.Children().Append(Label(L"Widget style", 19));

        auto styleCombos = Grid(); styleCombos.ColumnSpacing(12); styleCombos.RowSpacing(16);
        auto theme = ComboBox();
        auto bars = ComboBox();
        auto thickness = ComboBox();
        auto providerSpacing = ComboBox();
        auto fontSize = ComboBox();
        auto addCombo = [this](Panel const& parent, ComboBox const& combo, std::wstring const& title,
            std::wstring const& key, std::vector<std::pair<std::wstring, std::wstring>> const& values)
        {
            auto group = StackPanel(); group.Spacing(6); group.Children().Append(Label(title));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(combo, title);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(combo, std::wstring(L"Appearance_") + key);
            combo.MinWidth(250); combo.MaxWidth(360); combo.HorizontalAlignment(HorizontalAlignment::Left);
            for (auto const& [value, label] : values)
            {
                auto item = ComboBoxItem(); item.Content(box_value(NativeLocalization::Translate(label, g_nativeLanguage))); item.Tag(box_value(value)); combo.Items().Append(item);
            }
            const auto current = Text(key);
            for (uint32_t i = 0; i < combo.Items().Size(); ++i)
                if (unbox_value<hstring>(combo.Items().GetAt(i).as<ComboBoxItem>().Tag()) == current) combo.SelectedIndex(i);
            appearancePresetCombos_.emplace_back(key, combo);
            combo.SelectionChanged([this, key](auto const& sender, auto const&)
            {
                auto control = sender.as<ComboBox>();
                if (!hydrating_ && control.SelectedItem()) SetValue(key, JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag())));
            });
            group.Children().Append(combo); parent.Children().Append(group);
        };
        addCombo(styleCombos, theme, NativeLocalization::Translate(L"Theme", g_nativeLanguage), L"palette", {{L"system",NativeLocalization::Translate(L"Follow Windows", g_nativeLanguage)},{L"high_contrast_dark",NativeLocalization::Translate(L"Dark", g_nativeLanguage)},{L"high_contrast_light",NativeLocalization::Translate(L"Light", g_nativeLanguage)}});
        addCombo(styleCombos, bars, NativeLocalization::Translate(L"Progress bars", g_nativeLanguage), L"bar_style", {{L"continuous",NativeLocalization::Translate(L"Continuous", g_nativeLanguage)},{L"segmented",NativeLocalization::Translate(L"Segmented", g_nativeLanguage)}});
        addCombo(styleCombos, thickness, NativeLocalization::Translate(L"Bar thickness", g_nativeLanguage), L"bar_thickness", {{L"standard",NativeLocalization::Translate(L"Thick", g_nativeLanguage)},{L"slim",NativeLocalization::Translate(L"Slim", g_nativeLanguage)}});
        addCombo(styleCombos, providerSpacing, NativeLocalization::Translate(L"Provider spacing", g_nativeLanguage), L"provider_spacing", {{L"none",NativeLocalization::Translate(L"None", g_nativeLanguage)},{L"small",NativeLocalization::Translate(L"Small", g_nativeLanguage)},{L"normal",NativeLocalization::Translate(L"Normal", g_nativeLanguage)},{L"large",NativeLocalization::Translate(L"Large", g_nativeLanguage)}});
        providerSpacingSelector_ = providerSpacing;
        addCombo(styleCombos, fontSize, NativeLocalization::Translate(L"Text size", g_nativeLanguage), L"font_size", {{L"standard",NativeLocalization::Translate(L"Standard", g_nativeLanguage)},{L"large",NativeLocalization::Translate(L"Large", g_nativeLanguage)}});
        styleCombos.SizeChanged([styleCombos](auto const& sender, auto const&) mutable
        {
            const auto width = sender.as<Grid>().ActualWidth();
            const int columns = width >= 900 ? 3 : width >= 560 ? 2 : 1;
            styleCombos.ColumnDefinitions().Clear(); styleCombos.RowDefinitions().Clear();
            for (int c = 0; c < columns; ++c) styleCombos.ColumnDefinitions().Append(ColumnDefinition());
            const uint32_t childCount = styleCombos.Children().Size();
            const int rows = static_cast<int>((childCount + static_cast<uint32_t>(columns) - 1) / static_cast<uint32_t>(columns));
            for (int r = 0; r < rows; ++r) styleCombos.RowDefinitions().Append(RowDefinition());
            for (uint32_t i = 0; i < childCount; ++i)
            {
                Grid::SetColumn(styleCombos.Children().GetAt(i).as<FrameworkElement>(), static_cast<int>(i) % columns);
                Grid::SetRow(styleCombos.Children().GetAt(i).as<FrameworkElement>(), static_cast<int>(i) / columns);
            }
        });
        style.Children().Append(styleCombos);
        auto labelSizeGroup = StackPanel(); labelSizeGroup.Spacing(6);
        labelSizeGroup.Children().Append(Label(L"Usage window label size"));
        auto labelSizeNote = Label(L"Sets the 5-hour and 7-day labels in the widget using normal weight.", 12);
        labelSizeNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        labelSizeGroup.Children().Append(labelSizeNote);
        auto labelSize = ComboBox();
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(labelSize, NativeLocalization::Translate(L"Usage window label size", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(labelSize, L"Appearance_window_label_font_size");
        labelSize.MinWidth(250); labelSize.MaxWidth(360); labelSize.HorizontalAlignment(HorizontalAlignment::Left);
        for (int32_t size = 8; size <= 14; ++size)
        {
            auto item = ComboBoxItem(); item.Content(box_value(std::to_wstring(size) + L" pt")); item.Tag(box_value(size)); labelSize.Items().Append(item);
        }
        const auto currentLabelSize = static_cast<int32_t>(State().GetNamedNumber(L"window_label_font_size", 9));
        labelSize.SelectedIndex(std::clamp(currentLabelSize, 8, 14) - 8);
        appearancePresetCombos_.emplace_back(L"window_label_font_size", labelSize);
        labelSize.SelectionChanged([this](auto const& sender, auto const&)
        {
            auto control = sender.as<ComboBox>();
            if (!hydrating_ && control.SelectedItem())
                SetValue(L"window_label_font_size", JsonValue::CreateNumberValue(unbox_value<int32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));
        });
        labelSizeGroup.Children().Append(labelSize); style.Children().Append(labelSizeGroup);
        AddAccentColorRow(style, L"usage_text_color", L"Provider logo and percentage color");

        // The live preview now leads the page; see the top of BuildAppearance.

        styleCard.Child(style); AppendWidgetDependentCard(styleCard);

        auto actionsCard = Card();
        auto actions = StackPanel(); actions.Spacing(12);
        actions.Children().Append(Label(L"Appearance actions", 19));
        auto actionsNote = Label(L"Reset appearance or colors.", 12);
        actionsNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        actions.Children().Append(actionsNote);
        auto buttons = StackPanel(); buttons.Orientation(Orientation::Horizontal); buttons.Spacing(8);
        buttons.SizeChanged([](auto const& sender, auto const&)
        {
            auto panel = sender.as<StackPanel>();
            panel.Orientation(panel.ActualWidth() < 480 ? Orientation::Vertical : Orientation::Horizontal);
        });
        for (auto [label, action] : std::array<std::pair<std::wstring, std::wstring>, 2>{{
            {L"Reset appearance",L"reset_appearance"},{L"Reset colors",L"reset_colors"}}})
        {
            auto button = Button(); button.Content(box_value(NativeLocalization::Translate(label, g_nativeLanguage)));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(button, std::wstring(L"AppearanceAction_") + action);
            button.Click([this, action](auto const&, auto const&) { Action(action); }); buttons.Children().Append(button);
        }
        actions.Children().Append(buttons);
        actionsCard.Child(actions); AppendWidgetDependentCard(actionsCard);

        if (pageScroll_ && pageScroll_.VerticalOffset() > 8)
        {
            auto weak = get_weak();
            dispatcher_.TryEnqueue([weak]()
            {
                if (auto self = weak.get()) self->UpdateStickyPreview();
            });
        }
        BuildAppearancePresets();
    }

    void MainWindow::BuildProviderDetail(int provider)
    {
        const auto metas = ProviderAppearanceMeta();
        if (provider < 0 || provider >= static_cast<int>(metas.size()))
        {
            BuildAppearance();
            return;
        }
        const auto& meta = metas[static_cast<size_t>(provider)];
        if (!Bool(L"widget_visible")) AddWidgetHiddenWarning();
        BuildPreviewCard();
        auto titleCard = Card();
        auto titleRow = StackPanel(); titleRow.Orientation(Orientation::Horizontal); titleRow.Spacing(8);
        titleRow.VerticalAlignment(VerticalAlignment::Center);
        auto back = Button(); back.Content(SymbolIcon(Symbol::Back));
        back.Background(nullptr); back.BorderBrush(nullptr); back.BorderThickness(Thickness{ 0 });
        back.VerticalAlignment(VerticalAlignment::Center);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(back, NativeLocalization::Translate(L"Back to providers", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(back, L"ProviderDetailBack");
        back.Click([this](auto const&, auto const&)
        {
            detailProvider_ = -1;
            RenderPage(L"Appearance");
            if (pageScroll_) pageScroll_.ScrollToVerticalOffset(0);
        });
        titleRow.Children().Append(back);
        auto detailName = Label(meta.name, 19); detailName.VerticalAlignment(VerticalAlignment::Center);
        titleRow.Children().Append(detailName);
        titleCard.Padding(Thickness{ 12, 6, 12, 6 });
        titleCard.Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
        titleCard.Child(titleRow);
        appearanceDetailTitleCard_ = titleCard;
        PageHost().Children().Append(titleCard);
        auto card = Card(); auto stack = StackPanel(); stack.Spacing(12);
        const bool enabled = Bool(meta.enable);
        const bool available = state_ && state_.GetNamedBoolean(hstring(meta.availability), false);
        auto enableCopy = StackPanel(); enableCopy.Spacing(3); enableCopy.Children().Append(Label(L"Show in widget"));
        if (!available)
        {
            auto availabilityNote = Label(ProviderUnavailableDetail(meta, enabled), 12);
            availabilityNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            enableCopy.Children().Append(availabilityNote);
        }
        auto enableToggle = ToggleSwitch(); SetSwitchContent(enableToggle);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(enableToggle, meta.name);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(enableToggle, std::wstring(L"Setting_") + meta.enable);
        enableToggle.IsOn(enabled); enableToggle.IsEnabled(available || enabled);
        providerDetailEnableToggle_ = enableToggle;
        enableToggle.Toggled([this, meta](auto const& sender, auto const&)
        {
            auto control = sender.as<ToggleSwitch>();
            if (hydrating_) return;
            const bool enabledNow = control.IsOn();
            SetValue(meta.enable, JsonValue::CreateBooleanValue(enabledNow));
            const bool availableNow = state_ && state_.GetNamedBoolean(hstring(meta.availability), false);
            control.IsEnabled(availableNow || enabledNow);
        });
        stack.Children().Append(ToggleRow(enableCopy, enableToggle));
        auto iconCopy = StackPanel(); iconCopy.Spacing(3); iconCopy.Children().Append(Label(L"Provider logo"));
        auto iconToggle = ToggleSwitch(); SetSwitchContent(iconToggle);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(iconToggle, NativeLocalization::Format(L"%s logo", g_nativeLanguage, meta.name));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(iconToggle, std::wstring(L"Setting_") + meta.icon);
        iconToggle.IsOn(State().GetNamedBoolean(meta.icon, true));
        iconToggle.IsEnabled(Bool(L"show_provider_icon"));
        iconToggle.Toggled([this, meta](auto const& sender, auto const&)
        {
            auto control = sender.as<ToggleSwitch>();
            if (hydrating_) return;
            SetValue(meta.icon, JsonValue::CreateBooleanValue(control.IsOn()));
        });
        stack.Children().Append(ToggleRow(iconCopy, iconToggle));
        AddAccentColorRow(stack, meta.accent, NativeLocalization::Format(L"%s bar accent", g_nativeLanguage, meta.name));
        card.Child(stack); AppendWidgetDependentCard(card);
    }

    void MainWindow::ClearStickyPreview()
    {
        appearancePreviewFloating_ = false;
        appearanceTitleFloating_ = false;
        appearancePreviewFloatOffset_ = 0;
        if (appearancePreviewCard_)
        {
            uint32_t index{};
            if (StickyPreviewHost().Children().IndexOf(appearancePreviewCard_, index))
                StickyPreviewHost().Children().RemoveAt(index);
            appearancePreviewCard_ = nullptr;
        }
        if (appearanceDetailTitleCard_)
        {
            uint32_t index{};
            if (StickyPreviewHost().Children().IndexOf(appearanceDetailTitleCard_, index))
                StickyPreviewHost().Children().RemoveAt(index);
            appearanceDetailTitleCard_ = nullptr;
        }
        if (appearancePreviewPlaceholder_)
        {
            uint32_t placeholderIndex{};
            if (PageHost().Children().IndexOf(appearancePreviewPlaceholder_, placeholderIndex))
                PageHost().Children().RemoveAt(placeholderIndex);
            appearancePreviewPlaceholder_ = nullptr;
        }
        if (appearanceDetailTitlePlaceholder_)
        {
            uint32_t placeholderIndex{};
            if (PageHost().Children().IndexOf(appearanceDetailTitlePlaceholder_, placeholderIndex))
                PageHost().Children().RemoveAt(placeholderIndex);
            appearanceDetailTitlePlaceholder_ = nullptr;
        }
        StickyPreviewHost().Visibility(Visibility::Collapsed);
    }

    void MainWindow::UpdateStickyPreview()
    {
        if (closed_ || page_ != L"Appearance" || !pageScroll_) return;
        try
        {
            if (appearancePreviewCard_ && !appearancePreviewFloating_ && pageScroll_.VerticalOffset() > 8)
            {
                Windows::Foundation::Point origin{ 0, 0 };
                const auto top = appearancePreviewCard_.TransformToVisual(pageScroll_).TransformPoint(origin).Y;
                if (top <= 0)
                {
                    uint32_t index{};
                    if (PageHost().Children().IndexOf(appearancePreviewCard_, index))
                    {
                        const auto cardHeight = appearancePreviewCard_.ActualHeight();
                        const auto cardMargin = appearancePreviewCard_.Margin();
                        PageHost().Children().RemoveAt(index);
                        if (cardHeight > 0)
                        {
                            auto placeholder = Controls::Border();
                            placeholder.Height(cardHeight);
                            placeholder.Margin(cardMargin);
                            placeholder.Background(nullptr);
                            PageHost().Children().InsertAt(index, placeholder);
                            appearancePreviewPlaceholder_ = placeholder;
                        }
                        StickyPreviewHost().Children().Append(appearancePreviewCard_);
                        StickyPreviewHost().Visibility(Visibility::Visible);
                        appearancePreviewCard_.Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
                        appearancePreviewFloating_ = true;
                        appearancePreviewFloatOffset_ = pageScroll_.VerticalOffset();
                    }
                }
            }
            if (appearanceDetailTitleCard_ && !appearanceTitleFloating_ && pageScroll_.VerticalOffset() > 8)
            {
                Windows::Foundation::Point origin{ 0, 0 };
                const auto top = appearanceDetailTitleCard_.TransformToVisual(pageScroll_).TransformPoint(origin).Y;
                if (top <= 0)
                {
                    uint32_t index{};
                    if (PageHost().Children().IndexOf(appearanceDetailTitleCard_, index))
                    {
                        const auto cardHeight = appearanceDetailTitleCard_.ActualHeight();
                        const auto cardMargin = appearanceDetailTitleCard_.Margin();
                        PageHost().Children().RemoveAt(index);
                        if (cardHeight > 0)
                        {
                            auto placeholder = Controls::Border();
                            placeholder.Height(cardHeight);
                            placeholder.Margin(cardMargin);
                            placeholder.Background(nullptr);
                            PageHost().Children().InsertAt(index, placeholder);
                            appearanceDetailTitlePlaceholder_ = placeholder;
                        }
                        StickyPreviewHost().Children().Append(appearanceDetailTitleCard_);
                        StickyPreviewHost().Visibility(Visibility::Visible);
                        appearanceTitleFloating_ = true;
                    }
                }
            }
            if ((appearancePreviewFloating_ || appearanceTitleFloating_) &&
                pageScroll_.VerticalOffset() <= appearancePreviewFloatOffset_ - 32)
            {
                uint32_t index{};
                if (appearanceTitleFloating_ && appearanceDetailTitleCard_ && StickyPreviewHost().Children().IndexOf(appearanceDetailTitleCard_, index))
                    StickyPreviewHost().Children().RemoveAt(index);
                if (appearancePreviewFloating_ && appearancePreviewCard_ && StickyPreviewHost().Children().IndexOf(appearancePreviewCard_, index))
                    StickyPreviewHost().Children().RemoveAt(index);
                if (appearancePreviewPlaceholder_)
                {
                    uint32_t placeholderIndex{};
                    if (PageHost().Children().IndexOf(appearancePreviewPlaceholder_, placeholderIndex))
                        PageHost().Children().RemoveAt(placeholderIndex);
                    appearancePreviewPlaceholder_ = nullptr;
                }
                if (appearanceDetailTitlePlaceholder_)
                {
                    uint32_t placeholderIndex{};
                    if (PageHost().Children().IndexOf(appearanceDetailTitlePlaceholder_, placeholderIndex))
                        PageHost().Children().RemoveAt(placeholderIndex);
                    appearanceDetailTitlePlaceholder_ = nullptr;
                }
                if (appearancePreviewFloating_ && appearancePreviewCard_)
                {
                    PageHost().Children().InsertAt(0, appearancePreviewCard_);
                    appearancePreviewCard_.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
                }
                if (appearanceTitleFloating_ && appearanceDetailTitleCard_)
                {
                    if (PageHost().Children().Size() >= 1)
                        PageHost().Children().InsertAt(1, appearanceDetailTitleCard_);
                    else
                        PageHost().Children().Append(appearanceDetailTitleCard_);
                }
                StickyPreviewHost().Visibility(Visibility::Collapsed);
                appearancePreviewFloating_ = false;
                appearanceTitleFloating_ = false;
                appearancePreviewFloatOffset_ = 0;
            }
        }
        catch (...) {}
    }

    void MainWindow::BuildPlacement()
    {
        if (!Bool(L"widget_visible")) AddWidgetHiddenWarning();
        auto card=Card();auto stack=StackPanel();stack.Spacing(16);stack.Children().Append(Label(L"Taskbar placement",19));stack.Children().Append(Label(L"Choose the display where the widget should appear. The widget stays aligned with the taskbar.",13));auto monitors=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(monitors,NativeLocalization::Translate(L"Monitor device",g_nativeLanguage));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(monitors,L"MonitorSelector");monitors.MinWidth(250);monitors.MaxWidth(520);monitors.HorizontalAlignment(HorizontalAlignment::Left);auto automatic=ComboBoxItem();automatic.Content(box_value(NativeLocalization::Translate(L"Automatic (primary display)",g_nativeLanguage)));automatic.Tag(box_value(L""));monitors.Items().Append(automatic);if(state_&&state_.HasKey(L"monitors"))for(auto const& v:state_.GetNamedArray(L"monitors")){auto m=v.GetObject();auto device=m.GetNamedString(L"device",L"");auto item=ComboBoxItem();item.Content(box_value(NativeLocalization::Translate(m.GetNamedBoolean(L"primary",false)?L"Primary display · ":L"Display · ",g_nativeLanguage)+device));item.Tag(box_value(device));monitors.Items().Append(item);}monitors.SelectedIndex(0);auto current=Text(L"monitor_device");if(state_&&State().HasKey(L"monitor_device")&&State().GetNamedValue(L"monitor_device").ValueType()==JsonValueType::String)current=std::wstring(State().GetNamedString(L"monitor_device").c_str());for(uint32_t i=0;i<monitors.Items().Size();++i)if(std::wstring(unbox_value<hstring>(monitors.Items().GetAt(i).as<ComboBoxItem>().Tag()).c_str())==current)monitors.SelectedIndex(i);monitors.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem()){auto val=unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag());SetValue(L"monitor_device",val.empty()?JsonValue::CreateNullValue():JsonValue::CreateStringValue(val));}});stack.Children().Append(monitors);
          auto sideCopy=StackPanel();sideCopy.Spacing(3);sideCopy.Children().Append(Label(L"Anchor to the left"));auto sideDetail=Label(L"Place the widget at the left edge of the taskbar.",12);sideDetail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());sideCopy.Children().Append(sideDetail);auto side=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(side,NativeLocalization::Translate(L"Anchor to the left",g_nativeLanguage));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(side,L"Setting_anchor_left");SetSwitchContent(side);side.IsOn(Bool(L"anchor_left"));side.Toggled([this](auto const&sender,auto const&){auto control=sender.as<ToggleSwitch>();SetValue(L"anchor_left",JsonValue::CreateBooleanValue(control.IsOn()));});stack.Children().Append(ToggleRow(sideCopy,side));auto reset=Button();reset.Content(box_value(NativeLocalization::Translate(L"Reset position",g_nativeLanguage)));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(reset,L"ResetPlacement");reset.HorizontalAlignment(HorizontalAlignment::Left);reset.Click([this](auto const&,auto const&){Action(L"reset_position");});stack.Children().Append(reset);card.Child(stack);AppendWidgetDependentCard(card);
    }

    void MainWindow::NavigateTo(std::wstring const& page)
    {
        for (auto const& item : Navigation().MenuItems()) if (auto nav = item.try_as<NavigationViewItem>()) { if (std::wstring(unbox_value<hstring>(nav.Tag()).c_str()) == page) { Navigation().SelectedItem(item); return; } }
        for (auto const& item : Navigation().FooterMenuItems()) if (auto nav = item.try_as<NavigationViewItem>()) { if (std::wstring(unbox_value<hstring>(nav.Tag()).c_str()) == page) { Navigation().SelectedItem(item); return; } }
    }

    void MainWindow::BuildExperimental()
    {
        auto card = Card(); auto stack = StackPanel(); stack.Spacing(12);
        stack.Children().Append(Label(L"Provider limit", 19));
        stack.Children().Append(Label(L"Try upcoming ideas early. These options may change or disappear in a later version.", 13));
        auto copy = StackPanel(); copy.Spacing(3);
        copy.Children().Append(Label(L"Allow unlimited providers"));
        auto detail = Label(L"By default, up to 3 providers can be enabled at once. Turn this on to remove that limit.", 12);
        detail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        copy.Children().Append(detail);
        auto toggle = ToggleSwitch();
         Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(toggle, NativeLocalization::Translate(L"Allow unlimited providers", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(toggle, L"Setting_allow_unlimited_providers");
        SetSwitchContent(toggle); toggle.IsOn(Bool(L"allow_unlimited_providers"));
        toggle.Toggled([this](auto const& sender, auto const&) { auto control = sender.as<ToggleSwitch>(); SetValue(L"allow_unlimited_providers", JsonValue::CreateBooleanValue(control.IsOn())); });
        stack.Children().Append(ToggleRow(copy, toggle));
        card.Child(stack); PageHost().Children().Append(card);
    }

#if QUOTA_DEVELOPMENT_UI
    void MainWindow::OpenDevelopment()
    {
        if (state_ && !state_.GetNamedBoolean(L"development_enabled", false)) return;
        if (!developmentNavigationItem_)
        {
            developmentNavigationItem_ = NavigationViewItem();
            developmentNavigationItem_.Content(box_value(L"Development"));
            developmentNavigationItem_.Tag(box_value(L"Development"));
            developmentNavigationItem_.Icon(SymbolIcon(Symbol::Repair));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(developmentNavigationItem_, L"NavigationDevelopment");
            Navigation().FooterMenuItems().Append(developmentNavigationItem_);
        }
        NavigateTo(L"Development");
    }

    void MainWindow::QueueDevelopmentRequest(JsonObject const& request)
    {
        if (!state_ || !state_.GetNamedBoolean(L"development_enabled", false) || stopping_) return;
        if (developmentRequests_.size() >= 32)
        {
            ShowMessage(L"Development requests are still applying. Try again in a moment.", true);
            return;
        }
        developmentRequests_.push(request);
        SendNextDevelopmentRequest();
    }

    void MainWindow::SendNextDevelopmentRequest()
    {
        if (stopping_ || closed_ || closeRequested_ || inFlight_ != 0 || developmentRequests_.empty()) return;
        auto request = developmentRequests_.front();
        developmentRequests_.pop();
        developmentRequestInFlight_ = true;
        developmentResetInFlight_ = request.GetNamedString(L"op", L"") == L"development_reset";
        Send(request);
    }

    void MainWindow::BuildDevelopment()
    {
        PageEyebrow().Text(L"DEBUG BUILD ONLY");
        PageSummary().Text(L"Simulate providers and usage without changing credentials or saved settings.");
        SaveHint().Text(L"Use Apply on each provider. Overrides affect the widget and previews immediately and disappear when the app exits.");
        auto intro = Card();
        auto introContent = StackPanel(); introContent.Spacing(12);
        introContent.Children().Append(Label(L"Temporary development overrides", 19));
        introContent.Children().Append(Label(L"Force enable bypasses local provider detection and the provider count limit. Blank percentages keep the live quota value. Values are used percentages, from 0 to 100.", 13));
        auto reset = Button(); reset.Content(box_value(L"Reset all overrides"));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(reset, L"DevelopmentReset");
        reset.Click([this](auto const&, auto const&) { QueueDevelopmentRequest(Request(L"development_reset")); });
        introContent.Children().Append(reset); intro.Child(introContent); PageHost().Children().Append(intro);

        const std::array<std::wstring, 7> ids{ L"claude", L"codex", L"antigravity", L"opencode", L"cursor", L"grok", L"copilot" };
        const auto providers = ProviderAppearanceMeta();
        const auto overrides = state_ ? state_.GetNamedObject(L"development_overrides", JsonObject()) : JsonObject();
        for (size_t i = 0; i < providers.size(); ++i)
        {
            const auto id = ids[i];
            const auto current = overrides.GetNamedObject(id, JsonObject());
            auto card = Card(); auto content = StackPanel(); content.Spacing(12);
            content.Children().Append(Label(providers[i].setupName, 19));
            auto enabled = ToggleSwitch(); SetSwitchContent(enabled);
            enabled.IsOn(current.GetNamedBoolean(L"enabled", false));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(enabled, L"Use development override for " + id);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(enabled, L"Development_enabled_" + id);
            auto enabledCopy = StackPanel(); enabledCopy.Children().Append(Label(L"Use development override"));
            content.Children().Append(ToggleRow(enabledCopy, enabled));
            auto force = ToggleSwitch(); SetSwitchContent(force);
            force.IsOn(current.GetNamedBoolean(L"force_available", false));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(force, L"Force enable " + id);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(force, L"Development_force_" + id);
            auto forceCopy = StackPanel(); forceCopy.Children().Append(Label(L"Force enable provider"));
            content.Children().Append(ToggleRow(forceCopy, force));
            auto values = Grid(); values.ColumnSpacing(12);
            values.ColumnDefinitions().Append(ColumnDefinition()); values.ColumnDefinitions().Append(ColumnDefinition());
            std::array<NumberBox, 2> inputs;
            const std::array<std::wstring, 2> keys{ L"session_percent", L"weekly_percent" };
            for (size_t window = 0; window < inputs.size(); ++window)
            {
                auto input = NumberBox(); input.Minimum(0); input.Maximum(100);
                input.Header(box_value(window == 0 ? L"5-hour usage (%)" : L"7-day usage (%)"));
                input.PlaceholderText(L"Live value");
                input.SpinButtonPlacementMode(NumberBoxSpinButtonPlacementMode::Compact);
                const auto value = current.GetNamedValue(keys[window], JsonValue::CreateNullValue());
                input.Value(value.ValueType() == JsonValueType::Number ? value.GetNumber() : std::numeric_limits<double>::quiet_NaN());
                Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(input, L"Development_" + keys[window] + L"_" + id);
                Grid::SetColumn(input, static_cast<int>(window)); values.Children().Append(input); inputs[window] = input;
            }
            content.Children().Append(values);
            auto apply = Button(); apply.Content(box_value(L"Apply"));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(apply, L"Apply overrides for " + id);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(apply, L"Development_apply_" + id);
            apply.Click([this, id, enabled, force, inputs, keys](auto const&, auto const&)
            {
                auto request = Request(L"development_override");
                request.Insert(L"provider", JsonValue::CreateStringValue(id));
                request.Insert(L"enabled", JsonValue::CreateBooleanValue(enabled.IsOn()));
                request.Insert(L"force_available", JsonValue::CreateBooleanValue(force.IsOn()));
                for (size_t window = 0; window < inputs.size(); ++window)
                {
                    const double value = inputs[window].Value();
                    if (!std::isnan(value) && (!std::isfinite(value) || value < 0 || value > 100))
                    {
                        ShowMessage(L"Usage percentages must be between 0 and 100, or blank for live values.", true);
                        return;
                    }
                    request.Insert(keys[window], std::isnan(value) ? JsonValue::CreateNullValue() : JsonValue::CreateNumberValue(value));
                }
                QueueDevelopmentRequest(request);
            });
            content.Children().Append(apply); card.Child(content); PageHost().Children().Append(card);
        }
    }
#endif

    void MainWindow::BuildAbout()
    {
        auto hero = Card();
        auto h = StackPanel(); h.Spacing(18);
        auto nameRow = StackPanel(); nameRow.Orientation(Orientation::Horizontal); nameRow.Spacing(16); nameRow.VerticalAlignment(VerticalAlignment::Center);
        auto appIcon = Image(); appIcon.Width(60); appIcon.Height(60); appIcon.Stretch(Media::Stretch::Uniform); appIcon.VerticalAlignment(VerticalAlignment::Center);
        SetImageFromResource(appIcon, IDR_ABOUT_ICON);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(appIcon, NativeLocalization::Translate(L"Quota Usage app icon", g_nativeLanguage));
        auto identity = StackPanel(); identity.Spacing(2); identity.VerticalAlignment(VerticalAlignment::Center);
        auto appName = Label(L"Quota Usage", 27); appName.VerticalAlignment(VerticalAlignment::Center);
        appName.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
        auto author = Label(NativeLocalization::Translate(L"by Zoltán Rózsa", g_nativeLanguage), 13);
        author.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        identity.Children().Append(appName); identity.Children().Append(author);
        nameRow.Children().Append(appIcon); nameRow.Children().Append(identity);
        h.Children().Append(nameRow);
        auto descriptionPanel = Border();
        descriptionPanel.Background(Application::Current().Resources().Lookup(box_value(L"SolidBackgroundFillColorBaseBrush")).as<Media::Brush>());
        descriptionPanel.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
        descriptionPanel.BorderThickness(ThicknessHelper::FromUniformLength(1));
        descriptionPanel.CornerRadius(CornerRadiusHelper::FromUniformRadius(10));
        descriptionPanel.Padding(Thickness{ 16, 13, 16, 13 });
        auto description = Label(NativeLocalization::Translate(L"A lightweight, portable, easy-to-use, customizable, open-source usage monitor for Claude, Codex, and many other providers.", g_nativeLanguage), 14);
        description.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        descriptionPanel.Child(description);
        h.Children().Append(descriptionPanel);

        const auto currentVersion = state_ ? std::wstring(state_.GetNamedString(L"version", L"unknown").c_str()) : std::wstring(L"…");
        versionText_ = Label(L"v" + currentVersion, 12);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(versionText_,
            state_ ? NativeLocalization::Translate(L"Version", g_nativeLanguage) + L" " + currentVersion : NativeLocalization::Translate(L"Connecting to the widget…", g_nativeLanguage));
        versionText_.VerticalAlignment(VerticalAlignment::Center);
        versionText_.TextWrapping(TextWrapping::NoWrap);
        versionText_.TextTrimming(TextTrimming::CharacterEllipsis);
        versionText_.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());

        checkForUpdatesButton_ = Button();
        const bool updateAvailable = state_ && state_.GetNamedBoolean(L"update_available", false);
        const bool updateBusy = state_ && state_.GetNamedBoolean(L"update_busy", false);
        checkForUpdatesButton_.Content(box_value(NativeLocalization::Translate(updateBusy ? L"Working…" : updateAvailable ? L"Install update" : L"Check for updates", g_nativeLanguage)));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(checkForUpdatesButton_, L"CheckForUpdates");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(checkForUpdatesButton_,
            NativeLocalization::Translate(updateBusy ? L"Update in progress" : updateAvailable ? L"Install available update" : L"Check for updates", g_nativeLanguage));
        checkForUpdatesButton_.Style(Application::Current().Resources().Lookup(box_value(L"AccentButtonStyle")).as<Style>());
        checkForUpdatesButton_.IsEnabled(!updateBusy);
        checkForUpdatesButton_.HorizontalAlignment(HorizontalAlignment::Left);
        checkForUpdatesButton_.Click([this](auto const&, auto const&) { Action(L"update"); });
        auto updateActionRow = Grid(); updateActionRow.ColumnSpacing(12);
        updateActionRow.HorizontalAlignment(HorizontalAlignment::Stretch);
        auto actionColumn = ColumnDefinition(); actionColumn.Width(GridLength{ 1, GridUnitType::Auto });
        auto versionColumn = ColumnDefinition(); versionColumn.Width(GridLength{ 1, GridUnitType::Star });
        updateActionRow.ColumnDefinitions().Append(actionColumn);
        updateActionRow.ColumnDefinitions().Append(versionColumn);
        checkForUpdatesButton_.MaxWidth(240);
        checkForUpdatesButton_.VerticalAlignment(VerticalAlignment::Center);
        versionText_.MaxWidth(180);
        versionText_.HorizontalAlignment(HorizontalAlignment::Left);
        Grid::SetColumn(checkForUpdatesButton_, 0);
        Grid::SetColumn(versionText_, 1);
        updateActionRow.Children().Append(checkForUpdatesButton_);
        updateActionRow.Children().Append(versionText_);
        h.Children().Append(updateActionRow);

        auto separator = Border();
        separator.Height(1);
        separator.Background(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
        separator.HorizontalAlignment(HorizontalAlignment::Stretch);
        h.Children().Append(separator);

        auto footer = StackPanel(); footer.Spacing(12);
        footer.HorizontalAlignment(HorizontalAlignment::Stretch);
        auto supportLinks = StackPanel(); supportLinks.Spacing(8);
        auto supportHeading = Label(L"Project & licensing", 16);
        supportHeading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
        supportLinks.Children().Append(supportHeading);
        auto projectRow = StackPanel(); projectRow.Orientation(Orientation::Horizontal); projectRow.Spacing(12); projectRow.VerticalAlignment(VerticalAlignment::Center);
        auto repo = Button(); repo.Content(box_value(NativeLocalization::Translate(L"View project", g_nativeLanguage))); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(repo, L"ViewProject"); repo.HorizontalAlignment(HorizontalAlignment::Left); repo.Click([this](auto const&, auto const&) { Action(L"repository"); });
        projectRow.SizeChanged([](auto const& sender, auto const&)
        {
            auto row = sender.as<StackPanel>();
            row.Orientation(row.ActualWidth() < 420 ? Orientation::Vertical : Orientation::Horizontal);
        });
        auto license = Label(L"Licensed under AGPL-3.0-or-later", 13);
        license.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        license.VerticalAlignment(VerticalAlignment::Center);
        license.TextWrapping(TextWrapping::Wrap);
        projectRow.Children().Append(repo); projectRow.Children().Append(license); supportLinks.Children().Append(projectRow);
        auto noticesText = Label(L"Third-party license terms and required notices are included with this application.", 12);
        noticesText.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        supportLinks.Children().Append(noticesText);
        auto noticesButton = Button(); noticesButton.Content(box_value(NativeLocalization::Translate(L"Third-party licenses and notices", g_nativeLanguage)));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(noticesButton, L"ThirdPartyNotices");
        noticesButton.HorizontalAlignment(HorizontalAlignment::Left);
        noticesButton.VerticalAlignment(VerticalAlignment::Center);
        noticesButton.Click([this](auto const&, auto const&) { OpenThirdPartyNotices(); });

        auto roseLogo = Image(); roseLogo.Width(26); roseLogo.Height(26); roseLogo.Stretch(Media::Stretch::Uniform); roseLogo.VerticalAlignment(VerticalAlignment::Center);
        SetImageFromResource(roseLogo, IDR_DATAROSE_LOGO);
        auto dataroseName = Label(L"Datarose", 14); dataroseName.VerticalAlignment(VerticalAlignment::Center);
        dataroseName.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
        dataroseName.Foreground(Media::SolidColorBrush(Windows::UI::Color{ 255, 230, 95, 92 }));
        auto brand = StackPanel(); brand.Orientation(Orientation::Horizontal); brand.Spacing(8); brand.VerticalAlignment(VerticalAlignment::Center);
        brand.Children().Append(roseLogo); brand.Children().Append(dataroseName);
        auto developerLink = HyperlinkButton(); developerLink.Content(brand); developerLink.NavigateUri(Windows::Foundation::Uri(L"https://datarose.net"));
        developerLink.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
        developerLink.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
        developerLink.BorderThickness(ThicknessHelper::FromUniformLength(1));
        developerLink.Padding(Thickness{ 10, 7, 12, 7 });
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(developerLink, NativeLocalization::Translate(L"Visit Datarose website", g_nativeLanguage));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(roseLogo, NativeLocalization::Translate(L"Datarose logo", g_nativeLanguage));

        auto footerActions = Grid(); footerActions.ColumnSpacing(18); footerActions.RowSpacing(12);
        footerActions.HorizontalAlignment(HorizontalAlignment::Stretch);
        auto supportColumn = ColumnDefinition(); supportColumn.Width(GridLength{ 1, GridUnitType::Star });
        auto brandColumn = ColumnDefinition(); brandColumn.Width(GridLength{ 1, GridUnitType::Auto });
        footerActions.ColumnDefinitions().Append(supportColumn); footerActions.ColumnDefinitions().Append(brandColumn);
        footerActions.RowDefinitions().Append(RowDefinition());
        Grid::SetColumn(noticesButton, 0); Grid::SetRow(noticesButton, 0);
        Grid::SetColumn(developerLink, 1); Grid::SetRow(developerLink, 0);
        developerLink.HorizontalAlignment(HorizontalAlignment::Right);
        developerLink.VerticalAlignment(VerticalAlignment::Center);
        footerActions.Children().Append(noticesButton); footerActions.Children().Append(developerLink);
        footerActions.SizeChanged([footerActions, noticesButton, developerLink](auto const& sender, auto const&) mutable
        {
            const bool stacked = sender.as<Grid>().ActualWidth() < 450;
            footerActions.ColumnDefinitions().Clear(); footerActions.RowDefinitions().Clear();
            if (stacked)
            {
                auto column = ColumnDefinition(); column.Width(GridLength{ 1, GridUnitType::Star });
                footerActions.ColumnDefinitions().Append(column);
                footerActions.RowDefinitions().Append(RowDefinition()); footerActions.RowDefinitions().Append(RowDefinition());
                Grid::SetColumn(noticesButton, 0); Grid::SetRow(noticesButton, 0);
                Grid::SetColumn(developerLink, 0); Grid::SetRow(developerLink, 1);
                noticesButton.HorizontalAlignment(HorizontalAlignment::Left);
                developerLink.HorizontalAlignment(HorizontalAlignment::Right);
            }
            else
            {
                auto supportColumn = ColumnDefinition(); supportColumn.Width(GridLength{ 1, GridUnitType::Star });
                auto brandColumn = ColumnDefinition(); brandColumn.Width(GridLength{ 1, GridUnitType::Auto });
                footerActions.ColumnDefinitions().Append(supportColumn); footerActions.ColumnDefinitions().Append(brandColumn);
                footerActions.RowDefinitions().Append(RowDefinition());
                Grid::SetColumn(noticesButton, 0); Grid::SetRow(noticesButton, 0);
                Grid::SetColumn(developerLink, 1); Grid::SetRow(developerLink, 0);
                noticesButton.HorizontalAlignment(HorizontalAlignment::Left);
                developerLink.HorizontalAlignment(HorizontalAlignment::Right);
            }
        });
        footer.Children().Append(supportLinks);
        footer.Children().Append(footerActions);
        h.Children().Append(footer);
        hero.Child(h); PageHost().Children().Append(hero);

        auto how = Card(); auto howStack = StackPanel(); howStack.Spacing(10);
        howStack.Children().Append(Label(L"Designed to stay local", 19));
        howStack.Children().Append(Label(L"Quota Usage runs locally and reuses sign-ins from the Codex CLI and Claude Code CLI already installed on your PC. There is no separate Quota Usage account, login, or API-key setup.", 14));
        howStack.Children().Append(Label(L"It reads quota and reset information from provider services and shows the available usage windows in your taskbar. Quota requests go directly from this app to those services; they do not pass through a Quota Usage server.", 14));
        how.Child(howStack); PageHost().Children().Append(how);

        auto privacy = Card();
        auto privacyStack = StackPanel(); privacyStack.Spacing(14);
        privacyStack.Children().Append(Label(L"Privacy and Security", 19));
        privacyStack.Children().Append(Label(L"A brief overview of the data Quota Usage uses to display provider quotas and maintain your settings.", 13));

        auto addPrivacySection = [&privacyStack](std::wstring const& title, std::vector<std::wstring> const& items)
        {
            auto section = StackPanel(); section.Spacing(5);
            auto heading = Label(title, 14);
            heading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            section.Children().Append(heading);
            for (auto const& item : items) section.Children().Append(Label(std::wstring(L"•  ") + NativeLocalization::Translate(item, g_nativeLanguage), 13));
            privacyStack.Children().Append(section);
        };

        addPrivacySection(L"Sign-in data", {
            L"The app uses existing sign-ins for Claude Code, Codex, and Antigravity to retrieve quota information.",
            L"Claude Code credentials may be read from Windows or WSL; Antigravity credentials are read from Windows Credential Manager."
        });
        addPrivacySection(L"Network requests", {
            L"Quota requests go directly to the enabled providers; they do not pass through a Quota Usage service.",
            L"Update checks and portable update downloads contact GitHub."
        });
        addPrivacySection(L"Local settings", {
            L"Provider selection, widget placement, appearance, polling options, and the last update-check time are stored locally."
        });
        addPrivacySection(L"Account setup", {
            L"No Quota Usage account or separate API-key setup is required."
        });
        addPrivacySection(L"Credential refresh notes", {
            L"If a Claude Code or Codex token expires, the app may use the corresponding local CLI to refresh the sign-in.",
            L"To refresh an Antigravity sign-in, sign in again through Antigravity."
        });
        privacy.Child(privacyStack); PageHost().Children().Append(privacy);

         const auto channel = state_ ? state_.GetNamedString(L"install_channel", L"portable") : hstring(L"portable");
        auto uninstallCard = Card();
         uninstallCard.Background(Application::Current().Resources().Lookup(box_value(L"SystemFillColorCriticalBackgroundBrush")).as<Media::Brush>());
         uninstallCard.BorderBrush(Application::Current().Resources().Lookup(box_value(L"SystemFillColorCriticalBrush")).as<Media::Brush>());
        auto uninstallStack = StackPanel(); uninstallStack.Spacing(10);
        auto dangerHeading = Label(L"DANGER ZONE", 11);
        dangerHeading.FontWeight(Windows::UI::Text::FontWeights::SemiBold()); dangerHeading.CharacterSpacing(110);
         dangerHeading.Foreground(Application::Current().Resources().Lookup(box_value(L"SystemFillColorCriticalBrush")).as<Media::Brush>());
        uninstallStack.Children().Append(dangerHeading);
        uninstallStack.Children().Append(Label(L"Uninstall Quota Usage", 19));
        uninstallStack.Children().Append(Label(L"This removes the app and its local files. This action cannot be undone.", 14));
        uninstallDescription_ = Label(NativeLocalization::Translate(channel == L"winget"
            ? L"WinGet will open its uninstall flow and ask you to confirm."
            : L"The built-in flow will ask you to confirm before removing anything.", g_nativeLanguage), 13);
        uninstallDescription_.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        uninstallStack.Children().Append(uninstallDescription_);

        auto uninstall = Button();
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(uninstall, L"UninstallApp");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(uninstall, NativeLocalization::Translate(L"Uninstall Quota Usage", g_nativeLanguage));
        uninstall.Style(Navigation().Resources().Lookup(box_value(L"UninstallButtonStyle")).as<Style>());
        uninstall.HorizontalAlignment(HorizontalAlignment::Left);
        auto uninstallContent = StackPanel();
        uninstallContent.Orientation(Orientation::Horizontal); uninstallContent.Spacing(8);
        auto uninstallIcon = FontIcon(); uninstallIcon.Glyph(L"\uE74D"); uninstallIcon.FontSize(14);
        uninstallContent.Children().Append(uninstallIcon);
        uninstallContent.Children().Append(Label(L"Uninstall Quota Usage…", 14));
        uninstall.Content(uninstallContent);
        uninstall.Click([this](auto const&, auto const&) { Action(L"uninstall"); });
        uninstallStack.Children().Append(uninstall);
        uninstallCard.Child(uninstallStack);
        PageHost().Children().Append(uninstallCard);
    }

    void MainWindow::BuildUpdate()
    {
        auto card = Card();
        auto stack = StackPanel(); stack.Spacing(14);
        const bool available = state_ && state_.GetNamedBoolean(L"update_available", false);
        if (available)
        {
            auto badge = Border(); badge.Padding(Thickness{ 11, 5, 11, 5 });
            badge.CornerRadius(CornerRadiusHelper::FromUniformRadius(12));
            badge.Background(Application::Current().Resources().Lookup(box_value(L"AccentFillColorDefaultBrush")).as<Media::Brush>());
            auto badgeText = Label(L"UPDATE AVAILABLE", 11);
            badgeText.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            badgeText.Foreground(Application::Current().Resources().Lookup(box_value(L"TextOnAccentFillColorPrimaryBrush")).as<Media::Brush>());
            badge.Child(badgeText); stack.Children().Append(badge);
        }
        stack.Children().Append(Label(available ? L"A newer version is ready" : L"Check for an update", 21));
        updateStatusText_ = Label(state_ ? NativeLocalization::Translate(std::wstring(state_.GetNamedString(L"update_status", L"Update status unavailable").c_str()), g_nativeLanguage) : NativeLocalization::Translate(L"Checking update status…", g_nativeLanguage), 14);
        updateStatusText_.TextWrapping(TextWrapping::Wrap);
        stack.Children().Append(updateStatusText_);
        auto version = Label(state_ ? NativeLocalization::Translate(L"Installed version  ·  ", g_nativeLanguage) + NativeLocalization::Translate(std::wstring(state_.GetNamedString(L"version", L"unknown").c_str()), g_nativeLanguage) : NativeLocalization::Translate(L"Installed version is being checked…", g_nativeLanguage), 13);
        version.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        stack.Children().Append(version);
        checkForUpdatesButton_ = Button();
        checkForUpdatesButton_.Content(box_value(NativeLocalization::Translate(available ? L"Install update" : L"Check for updates", g_nativeLanguage)));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(checkForUpdatesButton_, L"CheckForUpdates");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(checkForUpdatesButton_, NativeLocalization::Translate(available ? L"Install available update" : L"Check for updates", g_nativeLanguage));
        checkForUpdatesButton_.Style(Application::Current().Resources().Lookup(box_value(L"AccentButtonStyle")).as<Style>());
        checkForUpdatesButton_.IsEnabled(!(state_ && state_.GetNamedBoolean(L"update_busy", false)));
        checkForUpdatesButton_.HorizontalAlignment(HorizontalAlignment::Left);
        checkForUpdatesButton_.Click([this](auto const&, auto const&) { Action(L"update"); });
        stack.Children().Append(checkForUpdatesButton_);
        card.Child(stack); PageHost().Children().Append(card);
    }
}
