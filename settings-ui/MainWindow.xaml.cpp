#include "pch.h"
#include "MainWindow.xaml.h"
#include "MainWindow.g.cpp"
#include "MainWindow.xaml.g.hpp"
#include "Resources.h"
#include <shellapi.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <iomanip>
#include <map>
#include <sstream>
#include <vector>

using namespace winrt;
using namespace Microsoft::UI::Xaml;
using namespace Microsoft::UI::Xaml::Controls;
using namespace Windows::Data::Json;

namespace
{
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
        auto label = TextBlock(); label.Text(text); label.FontSize(size); label.TextWrapping(TextWrapping::Wrap); return label;
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
        toggle.OnContent(box_value(L"On"));
        toggle.OffContent(box_value(L"Off"));
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
        auto updateContent = Grid();
        auto updateColumns = updateContent.ColumnDefinitions();
        updateColumns.Append(ColumnDefinition());
        updateColumns.Append(ColumnDefinition());
        updateColumns.GetAt(0).Width(GridLengthHelper::FromValueAndType(1, GridUnitType::Star));
        updateColumns.GetAt(1).Width(GridLengthHelper::Auto());
        auto updateLabel = TextBlock(); updateLabel.Text(L"Update");
        updateContent.Children().Append(updateLabel);
        updateIndicator_ = Border();
        updateIndicator_.Width(7); updateIndicator_.Height(7);
        updateIndicator_.CornerRadius(CornerRadiusHelper::FromUniformRadius(4));
        updateIndicator_.Background(Media::SolidColorBrush(Windows::UI::Color{ 255, 0, 120, 212 }));
        updateIndicator_.Margin(Thickness{ 10, 0, 12, 0 });
        updateIndicator_.VerticalAlignment(VerticalAlignment::Center);
        updateIndicator_.Visibility(Visibility::Collapsed);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(updateIndicator_, L"Update available");
        Grid::SetColumn(updateIndicator_, 1); updateContent.Children().Append(updateIndicator_);
        NavigationUpdate().Content(updateContent);
        NavigationUpdate().Visibility(Visibility::Collapsed);
        auto weak = get_weak();
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
            if (paneStateKnown_ && !hydrating_ && !Bool(L"settings_pane_open"))
                SetValue(L"settings_pane_open", JsonValue::CreateBooleanValue(true));
        });
        Navigation().PaneClosed([this](auto const&, auto const&)
        {
            if (paneStateKnown_ && !hydrating_ && Bool(L"settings_pane_open"))
                SetValue(L"settings_pane_open", JsonValue::CreateBooleanValue(false));
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
        if (highContrast)
        {
            SystemBackdrop(nullptr);
            auto color = GetSysColor(COLOR_WINDOW);
            Navigation().Background(Media::SolidColorBrush(Windows::UI::Color{ 255, GetRValue(color), GetGValue(color), GetBValue(color) }));
        }
        else
        {
            OSVERSIONINFOW version{ sizeof(version) };
            using RtlGetVersionFn = LONG(WINAPI*)(OSVERSIONINFOW*);
            auto ntdll = GetModuleHandleW(L"ntdll.dll");
            auto getVersion = reinterpret_cast<RtlGetVersionFn>(GetProcAddress(ntdll, "RtlGetVersion"));
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
        Navigation().SelectedItem(Navigation().MenuItems().GetAt(0));
        for (auto const& item : Navigation().MenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == requestedPage) Navigation().SelectedItem(item);
        for (auto const& item : Navigation().FooterMenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == requestedPage) Navigation().SelectedItem(item);
        if (pipeName_.empty()) ShowMessage(L"Settings could not connect to the widget. Open this window from the Codex Usage widget.", true);
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
                        self->ShowMessage(L"Could not connect to Codex Usage. Close this window and try again from the widget.", true);
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
        const bool dirty = !closed_ && (HasPendingChanges() || HasUnappliedColorDraft());
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
            ShowMessage(L"Saving changes…");
            Send(request);
            return;
        }
        if (flushRequested_) { flushRequested_ = false; ContinueDeferredWork(); }
        RefreshSettingsControlsIfSafe();
    }

    void MainWindow::ContinueDeferredWork()
    {
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
        dialog.Title(box_value(L"Changes could not be saved"));
        dialog.Content(box_value(L"Your changes are active in the widget but could not be saved. Retry to save them. Close without saving keeps the current values only for this run; they may be lost when Codex Usage exits. Cancel returns to settings."));
        dialog.PrimaryButtonText(L"Retry save");
        dialog.SecondaryButtonText(L"Close without saving");
        dialog.CloseButtonText(L"Cancel");
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
            ? L"Close canceled. The requested settings action is still waiting for a successful save. Current changes remain active; unsaved changes may be lost when Codex Usage exits."
            : L"Close canceled. Current changes remain active; unsaved changes may be lost when Codex Usage exits.", true);
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
                for (auto const& item : Navigation().MenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == target) Navigation().SelectedItem(item);
                for (auto const& item : Navigation().FooterMenuItems()) if (item.try_as<NavigationViewItem>() && std::wstring(unbox_value<hstring>(item.as<NavigationViewItem>().Tag()).c_str()) == target) Navigation().SelectedItem(item);
            }
            else if (event == L"state" && message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object)
            {
                auto updated = message.GetNamedObject(L"state");
                const auto revision = static_cast<uint64_t>(message.GetNamedNumber(L"revision", 0));
                bool settingsChanged = false;
                bool localEcho = false;
                if (revision >= revision_)
                {
                    settingsChanged = SettingsChanged(state_, updated);
                    auto localChanges = JsonObject();
                    if (flightChanges_) for (auto const& pair : flightChanges_) localChanges.Insert(pair.Key(), pair.Value());
                    if (pendingChanges_) for (auto const& pair : pendingChanges_) if (!localChanges.HasKey(pair.Key())) localChanges.Insert(pair.Key(), pair.Value());
                    localEcho = settingsChanged && SettingsChangesAreLocalEcho(state_, updated, localChanges);
                    state_ = updated;
                    revision_ = revision;
                    UpdateWidgetPreview(updated);
                }
                UpdateClaudeAvailability();
                UpdateAboutControls();
                if (settingsChanged) SyncPaneState();
                // Poll/status events are frequent; avoid rebuilding controls (and
                // stealing keyboard focus) when only non-setting state changed.
                // Local edits are already represented by the controls and the
                // draft overlay. Recreating the page here makes edits lose focus.
                bool refreshPage = false;
                if (settingsChanged && !localEcho)
                {
                    settingsRefreshPending_ = true;
                    refreshPage = true;
                }
                if (refreshPage) RefreshSettingsControlsIfSafe();
            }
            return;
        }
        if (!message.HasKey(L"id") || message.GetNamedNumber(L"id", 0) != inFlight_) return;
        const bool isInitialGet = initialGetPending_;
        const bool wasPreview = previewInFlight_;
        const auto completedPreviewGeneration = widgetPreviewFlightGeneration_;
        const bool wasMutation = mutationInFlight_;
        const bool wasSettingsAction = settingsActionInFlight_;
        const auto completedSettingsAction = settingsActionInFlightName_;
        initialGetPending_ = false;
        previewInFlight_ = false;
        settingsActionInFlight_ = false;
        settingsActionInFlightName_.reset();
        inFlight_ = 0;
        const bool ok = message.GetNamedBoolean(L"ok", false);
        if (wasPreview)
        {
            if (ok && completedPreviewGeneration == widgetPreviewGeneration_ &&
                message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object)
                UpdateWidgetPreview(message.GetNamedObject(L"state"));
            else if (!ok && completedPreviewGeneration == widgetPreviewGeneration_ && widgetPreviewStatus_)
                widgetPreviewStatus_.Text(L"The widget preview is temporarily unavailable.");

            if (previewPending_) SendWidgetPreviewIfIdle();
            else if (flushRequested_) SendNextMutation();
            else ContinueDeferredWork();
            RefreshSettingsControlsIfSafe();
            return;
        }
        const auto responseRevision = static_cast<uint64_t>(message.GetNamedNumber(L"revision", 0));
        if (message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object && responseRevision >= revision_)
        { state_ = message.GetNamedObject(L"state"); revision_ = responseRevision; UpdateWidgetPreview(state_); }
        if (isInitialGet)
        {
            SyncPaneState();
        }
        UpdateClaudeAvailability();
        UpdateAboutControls();
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
        else if (!ok && !wasMutation && !wasPersist) ShowMessage(std::wstring(message.GetNamedString(L"error", L"The request could not be completed.").c_str()), true);
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
        if (previewPending_) SendWidgetPreviewIfIdle();
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
        flushRequested_ = false;
        UpdateSaveBar();
        if (key == L"palette" || key == L"bar_style" || key == L"bar_thickness" || key == L"font_size" ||
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
    void MainWindow::Action(std::wstring const& action)
    {
        if (previewTimer_) previewTimer_.Stop();
        previewPending_ = false;
        deferredAction_ = action;
        FlushPending();
    }
    void MainWindow::ShowMessage(std::wstring const& text, bool error)
    {
        MessageBar().Message(text);
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
        if (widgetPreviewStatus_) widgetPreviewStatus_.Text(L"Updating the sample from the widget…");
        if (widgetPreviewImage_)
        {
            widgetPreviewImage_.Visibility(Visibility::Collapsed);
            displayedWidgetPreviewBase64_.clear();
        }
        if (previewTimer_) { previewTimer_.Stop(); previewTimer_.Start(); }
    }
    void MainWindow::SendWidgetPreviewIfIdle()
    {
        if (closed_ || stopping_ || page_ != L"Appearance" || !previewPending_ || inFlight_ != 0) return;
        if (previewTimer_) previewTimer_.Stop();
        auto settings = State();
        auto changes = JsonObject();
        static const std::array<std::wstring, 24> previewKeys{
            L"palette", L"bar_style", L"bar_thickness", L"font_size", L"window_label_font_size",
            L"claude_accent", L"codex_accent", L"antigravity_accent", L"opencode_accent",
            L"cursor_accent", L"grok_accent", L"copilot_accent", L"usage_text_color", L"show_provider_icon",
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
            widgetPreviewStatus_.Text(L"Waiting for the widget’s sample preview…");
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
                self->widgetPreviewImage_.Source(bitmap);
                self->widgetPreviewImage_.Width((self->widgetPreviewWidthPx_ ? self->widgetPreviewWidthPx_ : bitmap.PixelWidth()) / scale);
                self->widgetPreviewImage_.Height((self->widgetPreviewHeightPx_ ? self->widgetPreviewHeightPx_ : bitmap.PixelHeight()) / scale);
                self->widgetPreviewImage_.Visibility(Visibility::Visible);
                self->displayedWidgetPreviewBase64_ = displayKey;
                self->widgetPreviewStatus_.Text(L"Sample values rendered by the widget.");
            }
        }
        catch (...)
        {
            if (auto self = weak.get(); self && generation == self->widgetPreviewImageGeneration_ && self->widgetPreviewStatus_)
                self->widgetPreviewStatus_.Text(L"The widget preview is temporarily unavailable.");
        }
    }
    void MainWindow::UpdateClaudeAvailability()
    {
        if (!claudeToggle_) return;
        const bool available = state_ && state_.GetNamedBoolean(L"claude_code_available", false);
        claudeToggle_.IsEnabled(available || Bool(L"show_claude_code"));
    }
    void MainWindow::UpdateAboutControls()
    {
        if (versionText_)
            versionText_.Text(state_ ? std::wstring(L"Version ") + state_.GetNamedString(L"version", L"unknown").c_str() : L"Connecting to the widget…");
        if (updateStatusText_)
            updateStatusText_.Text(state_ ? state_.GetNamedString(L"update_status", L"Update status unavailable") : L"Checking update status…");
        UpdateUpdateIndicator();
        if (checkForUpdatesButton_)
        {
            const bool busy = state_ && state_.GetNamedBoolean(L"update_busy", false);
            const bool available = state_ && state_.GetNamedBoolean(L"update_available", false);
            checkForUpdatesButton_.IsEnabled(!busy);
            checkForUpdatesButton_.Content(box_value(busy ? L"Working…" : available ? L"Install update" : L"Check for updates"));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(checkForUpdatesButton_,
                busy ? L"Update in progress" : available ? L"Install available update" : L"Check for updates");
        }
        if (uninstallDescription_)
        {
            const auto channel = state_ ? state_.GetNamedString(L"install_channel", L"portable") : hstring(L"portable");
            uninstallDescription_.Text(channel == L"winget"
                ? L"WinGet will open its uninstall flow and ask you to confirm."
                : L"The built-in flow will ask you to confirm before removing anything.");
        }
    }
    void MainWindow::UpdateUpdateIndicator()
    {
        const bool available = state_ && state_.GetNamedBoolean(L"update_available", false);
        if (updateIndicator_) updateIndicator_.Visibility(available ? Visibility::Visible : Visibility::Collapsed);
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
        window.Title(L"Third-party licenses and notices");
        auto layout = Grid();
        layout.Padding(Thickness{ 24, 20, 24, 24 });
        auto rows = layout.RowDefinitions();
        rows.Append(RowDefinition());
        rows.Append(RowDefinition());
        rows.GetAt(0).Height(GridLengthHelper::Auto());
        rows.GetAt(1).Height(GridLengthHelper::FromValueAndType(1, GridUnitType::Star));
        auto heading = StackPanel(); heading.Spacing(5);
        heading.Children().Append(Label(L"Third-party licenses and notices", 22));
        auto intro = Label(L"Notices for components included with Codex Usage. The text below is provided as published.", 13);
        intro.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        intro.Margin(Thickness{ 0, 0, 0, 18 }); heading.Children().Append(intro);
        layout.Children().Append(heading);
        auto text = TextBox();
        text.IsReadOnly(true);
        text.AcceptsReturn(true);
        text.Text(ResourceText(IDR_THIRD_PARTY_NOTICES));
        text.TextWrapping(TextWrapping::Wrap);
        text.FontFamily(Media::FontFamily(L"Segoe UI"));
        text.FontSize(13);
        text.Padding(Thickness{ 18, 16, 18, 16 });
        text.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
        text.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
        text.BorderThickness(Thickness{ 1, 1, 1, 1 });
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(text, L"Third-party license and notice text");
        Grid::SetRow(text, 1);
        layout.Children().Append(text);
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
        if (!settingsRefreshPending_ || initialGetPending_ || inFlight_ != 0 || mutationInFlight_ || HasPendingChanges() || appearanceColorDraft_) return;
        settingsRefreshPending_ = false;
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
        page_ = page; PageHost().Children().Clear();
        ++widgetPreviewGeneration_;
        ++widgetPreviewImageGeneration_;
        displayedWidgetPreviewBase64_.clear();
        if (page != L"Appearance")
        {
            if (previewTimer_) previewTimer_.Stop();
            previewPending_ = false;
        }
        claudeToggle_ = nullptr;
        widgetPreviewImage_ = nullptr;
        widgetPreviewStatus_ = nullptr;
        ClearStickyPreview();
        versionText_ = nullptr;
        updateStatusText_ = nullptr;
        uninstallDescription_ = nullptr;
        checkForUpdatesButton_ = nullptr;
        PageTitle().Text(page == L"Update" ? L"Software update" : page);
        PageEyebrow().Text(page == L"About" ? L"ABOUT CODEX USAGE" : page == L"Update" ? L"KEEPING CURRENT" : L"PREFERENCES");
        const std::map<std::wstring, std::wstring> summaries{
            {L"General",L"Manage startup, taskbar visibility, and language."}, {L"Monitoring",L"Choose providers and refresh options."},
            {L"Appearance",L"Choose the widget’s theme and visual settings."}, {L"Placement",L"Choose the display and taskbar position."},
            {L"About",L"A closer look at the app, how it works, and its project details."},
            {L"Update",L"See the current update status and install the latest available version."}};
        PageSummary().Text(summaries.at(page));
        hydrating_ = true;
        if (page == L"General") BuildGeneral(); else if (page == L"Monitoring") BuildMonitoring(); else if (page == L"Appearance") BuildAppearance(); else if (page == L"Placement") BuildPlacement(); else if (page == L"Update") BuildUpdate(); else BuildAbout();
        hydrating_ = false;
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
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw, title);
            const auto automationId = key == L"show_provider_icon" ? std::wstring(L"Setting_show_provider_icons") : std::wstring(L"Setting_") + key;
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(sw, automationId);
            SetSwitchContent(sw);
            sw.IsOn(key == L"show_provider_icon" ? State().GetNamedBoolean(key, true) : Bool(key));
            sw.Toggled([this, key](auto const& sender, auto const&) { SetValue(key, JsonValue::CreateBooleanValue(sender.as<ToggleSwitch>().IsOn())); });
            stack.Children().Append(ToggleRow(copy, sw));
        };
        toggle(L"startup", L"Start with Windows", L"Open the widget when you sign in.");
        toggle(L"widget_visible", L"Show the taskbar widget", L"Keep the live usage view visible in your taskbar.");
        toggle(L"show_tray_icon", L"Show the Windows notification-area icon", L"Show or hide Codex Usage’s separate icon beside the Windows clock.");
        toggle(L"show_provider_icon", L"Show provider marks inside the widget", L"Show or hide the small provider icons in the usage widget; this does not affect the Windows notification-area icon.");
        auto languageRow = StackPanel(); languageRow.Spacing(8); languageRow.Children().Append(Label(L"Language"));
        auto languages = ComboBox(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(languages, L"Language"); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(languages, L"LanguageSelector"); languages.MinWidth(230); languages.HorizontalAlignment(HorizontalAlignment::Left);
        if (state_ && state_.HasKey(L"languages")) for (auto const& value : state_.GetNamedArray(L"languages"))
        {
            auto language = value.GetObject(); auto item = ComboBoxItem(); item.Content(box_value(language.GetNamedString(L"name", language.GetNamedString(L"code", L"")))); item.Tag(box_value(language.GetNamedString(L"code", L""))); languages.Items().Append(item);
        }
        auto system = ComboBoxItem(); system.Content(box_value(L"Use Windows language")); system.Tag(box_value(L"system")); languages.Items().InsertAt(0, system);
        languages.SelectedIndex(0);
        auto selectedLanguage = Text(L"language"); for (uint32_t i=0; i<languages.Items().Size(); ++i) if (unbox_value<hstring>(languages.Items().GetAt(i).as<ComboBoxItem>().Tag()) == selectedLanguage) languages.SelectedIndex(i);
        languages.SelectionChanged([this](auto const& sender, auto const&) { auto control=sender.as<ComboBox>(); if (!hydrating_ && control.SelectedItem()) SetValue(L"language", JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag()))); });
        languageRow.Children().Append(languages); stack.Children().Append(languageRow);
        card.Child(stack); PageHost().Children().Append(card);
        auto exitCard = Card(); auto exitStack = StackPanel(); exitStack.Spacing(8);
        exitStack.Children().Append(Label(L"Close the widget", 16));
        exitStack.Children().Append(Label(L"Codex Usage will stop running until you start it again.", 12));
        auto exit = Button(); exit.Content(box_value(L"Exit Codex Usage")); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(exit, L"ExitApp"); exit.Click([this](auto const&, auto const&) { Action(L"exit"); }); exitStack.Children().Append(exit);
        exitCard.Child(exitStack); PageHost().Children().Append(exitCard);
    }

    void MainWindow::BuildMonitoring()
    {
        auto providers=Card(); auto p=StackPanel(); p.Spacing(14); p.Children().Append(Label(L"Usage providers",19));
        auto available=state_ && state_.GetNamedBoolean(L"claude_code_available", false);
        auto add=[this,&p,available](std::wstring key,std::wstring name,std::wstring detail,bool enabled,bool unavailable)
        {
             auto copy=StackPanel(); copy.Spacing(3); copy.Children().Append(Label(name)); auto d=Label(unavailable?(enabled?L"Claude credentials are unavailable. Turn this off, or sign in to resume monitoring.":L"Sign in to the Claude CLI to enable monitoring."):detail,12); d.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>()); copy.Children().Append(d);
             auto sw=ToggleSwitch(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw,name); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(sw,std::wstring(L"Setting_")+key); SetSwitchContent(sw); sw.IsOn(enabled); sw.IsEnabled(!unavailable || enabled); sw.Toggled([this,key](auto const& s,auto const&){SetValue(key,JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));}); if(key==L"show_claude_code") claudeToggle_=sw; p.Children().Append(ToggleRow(copy,sw));
        };
        add(L"show_claude_code",L"Claude Code",L"Monitor usage from your Claude CLI account.",Bool(L"show_claude_code"),!available);
        add(L"show_codex",L"Codex",L"Monitor your Codex account.",Bool(L"show_codex"),false);
        add(L"show_antigravity",L"Antigravity",L"Monitor your Antigravity account.",Bool(L"show_antigravity"),false);
        auto note=Label(L"At least one provider must stay enabled.",12);note.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());p.Children().Append(note);providers.Child(p);PageHost().Children().Append(providers);
        auto widgetCard = Card(); auto widgetSettings = StackPanel(); widgetSettings.Spacing(14);
        widgetSettings.Children().Append(Label(L"Widget contents", 19));
        widgetSettings.Children().Append(Label(L"Choose what appears in the taskbar widget. Keep at least one usage value visible; provider marks alone do not count.", 12));
        auto addWidgetToggle = [this, &widgetSettings](std::wstring const& key, std::wstring const& title, std::wstring const& detail)
        {
            auto copy = StackPanel(); copy.Spacing(3); copy.Children().Append(Label(title));
            auto description = Label(detail, 12);
            description.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
            copy.Children().Append(description);
            auto toggle = ToggleSwitch(); SetSwitchContent(toggle);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(toggle, title);
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
        auto interval=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(interval,L"Polling interval");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(interval,L"PollingInterval");interval.MinWidth(220);interval.HorizontalAlignment(HorizontalAlignment::Left);
        for(auto [ms,label]:std::array<std::pair<uint32_t,std::wstring>,4>{{{60000,L"Every minute"},{300000,L"Every 5 minutes"},{900000,L"Every 15 minutes"},{3600000,L"Every hour"}}}){auto it=ComboBoxItem();it.Content(box_value(label));it.Tag(box_value(ms));interval.Items().Append(it);}
        auto current=State().GetNamedNumber(L"poll_interval_ms",900000);for(uint32_t i=0;i<interval.Items().Size();++i)if(unbox_value<uint32_t>(interval.Items().GetAt(i).as<ComboBoxItem>().Tag())==current)interval.SelectedIndex(i);
        interval.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(L"poll_interval_ms",JsonValue::CreateNumberValue(unbox_value<uint32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));});
        auto intervalLabel=StackPanel();intervalLabel.Spacing(7);intervalLabel.Children().Append(Label(L"Polling interval"));intervalLabel.Children().Append(interval);o.Children().Append(intervalLabel);
         auto adaptiveCopy=StackPanel();adaptiveCopy.Spacing(3);adaptiveCopy.Children().Append(Label(L"Adaptive refresh"));auto adaptiveDetail=Label(L"Adjust refresh timing to match recent usage activity.",12);adaptiveDetail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());adaptiveCopy.Children().Append(adaptiveDetail);
         auto adaptive=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(adaptive,L"Adaptive refresh");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(adaptive,L"Setting_adaptive_refresh");SetSwitchContent(adaptive);adaptive.IsOn(Bool(L"adaptive_refresh"));adaptive.Toggled([this](auto const&s,auto const&){SetValue(L"adaptive_refresh",JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));});o.Children().Append(ToggleRow(adaptiveCopy,adaptive));
        auto wins=StackPanel();wins.Spacing(8);wins.Children().Append(Label(L"Usage windows"));
         for(auto [key,title]:std::array<std::pair<std::wstring,std::wstring>,2>{{{L"show_session_window",L"Session"},{L"show_weekly_window",L"Weekly"}}}){auto copy=StackPanel();copy.Spacing(3);copy.Children().Append(Label(title));auto detail=Label(title==L"Session"?L"Show the current session usage window.":L"Show the weekly usage window.",12);detail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());copy.Children().Append(detail);auto sw=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw,title);Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(sw,std::wstring(L"Setting_")+key);SetSwitchContent(sw);sw.IsOn(Bool(key));sw.Toggled([this,key](auto const&s,auto const&){SetValue(key,JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));});wins.Children().Append(ToggleRow(copy,sw));}o.Children().Append(wins);
        auto alerts=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(alerts,L"Low quota alerts");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(alerts,L"LowQuotaAlertThreshold");alerts.MinWidth(220);alerts.HorizontalAlignment(HorizontalAlignment::Left);for(auto [n,label]:std::array<std::pair<int,std::wstring>,4>{{{0,L"Off"},{10,L"10% remaining"},{20,L"20% remaining"},{30,L"30% remaining"}}}){auto it=ComboBoxItem();it.Content(box_value(label));it.Tag(box_value(n));alerts.Items().Append(it);}auto threshold=State().GetNamedNumber(L"alert_threshold_percent",0);for(uint32_t i=0;i<alerts.Items().Size();++i)if(unbox_value<int32_t>(alerts.Items().GetAt(i).as<ComboBoxItem>().Tag())==threshold)alerts.SelectedIndex(i);alerts.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(L"alert_threshold_percent",JsonValue::CreateNumberValue(unbox_value<int32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));});auto ar=StackPanel();ar.Spacing(7);ar.Children().Append(Label(L"Low quota alerts"));ar.Children().Append(alerts);o.Children().Append(ar);
        auto refresh = Button(); refresh.Content(box_value(L"Refresh usage now")); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(refresh,L"RefreshUsage"); refresh.HorizontalAlignment(HorizontalAlignment::Left); refresh.Click([this](auto const&, auto const&) { Action(L"refresh"); }); o.Children().Append(refresh);
        options.Child(o);PageHost().Children().Append(options);
    }

    void MainWindow::BuildAppearance()
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
        widgetPreviewImage_.Stretch(Media::Stretch::None);
        widgetPreviewImage_.HorizontalAlignment(HorizontalAlignment::Left);
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(widgetPreviewImage_, L"Widget appearance preview with sample usage values");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(widgetPreviewImage_, L"WidgetPreviewImage");
        widgetPreviewImage_.Visibility(Visibility::Collapsed);
        previewStack.Children().Append(widgetPreviewImage_);
        widgetPreviewStatus_ = Label(L"Waiting for the widget’s sample preview…", 12);
        widgetPreviewStatus_.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(widgetPreviewStatus_, L"WidgetPreviewStatus");
        previewStack.Children().Append(widgetPreviewStatus_);
        previewCard.Child(previewStack); PageHost().Children().Append(previewCard);
        appearancePreviewCard_ = previewCard;
        ScheduleWidgetPreview();
        UpdateWidgetPreview(state_);

        auto styleCard = Card();
        auto style = StackPanel(); style.Spacing(16);
        style.Children().Append(Label(L"Widget style", 19));

        auto theme = ComboBox();
        auto bars = ComboBox();
        auto thickness = ComboBox();
        auto fontSize = ComboBox();
        auto addCombo = [this](StackPanel const& parent, ComboBox const& combo, std::wstring const& title,
            std::wstring const& key, std::vector<std::pair<std::wstring, std::wstring>> const& values)
        {
            auto group = StackPanel(); group.Spacing(6); group.Children().Append(Label(title));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(combo, title);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(combo, std::wstring(L"Appearance_") + key);
            combo.MinWidth(250); combo.MaxWidth(360); combo.HorizontalAlignment(HorizontalAlignment::Left);
            for (auto const& [value, label] : values)
            {
                auto item = ComboBoxItem(); item.Content(box_value(label)); item.Tag(box_value(value)); combo.Items().Append(item);
            }
            const auto current = Text(key);
            for (uint32_t i = 0; i < combo.Items().Size(); ++i)
                if (unbox_value<hstring>(combo.Items().GetAt(i).as<ComboBoxItem>().Tag()) == current) combo.SelectedIndex(i);
            combo.SelectionChanged([this, key](auto const& sender, auto const&)
            {
                auto control = sender.as<ComboBox>();
                if (!hydrating_ && control.SelectedItem()) SetValue(key, JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag())));
            });
            group.Children().Append(combo); parent.Children().Append(group);
        };
        addCombo(style, theme, L"Theme", L"palette", {{L"system",L"Follow Windows"},{L"high_contrast_dark",L"Dark"},{L"high_contrast_light",L"Light"}});
        addCombo(style, bars, L"Progress bars", L"bar_style", {{L"continuous",L"Continuous"},{L"segmented",L"Segmented"}});
        addCombo(style, thickness, L"Bar thickness", L"bar_thickness", {{L"standard",L"Standard"},{L"slim",L"Slim"}});
        addCombo(style, fontSize, L"Text size", L"font_size", {{L"standard",L"Standard"},{L"large",L"Large"}});
        auto labelSizeGroup = StackPanel(); labelSizeGroup.Spacing(6);
        labelSizeGroup.Children().Append(Label(L"Usage window label size"));
        auto labelSizeNote = Label(L"Sets the 5-hour and 7-day labels in the widget using normal weight.", 12);
        labelSizeNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        labelSizeGroup.Children().Append(labelSizeNote);
        auto labelSize = ComboBox();
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(labelSize, L"Usage window label size");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(labelSize, L"Appearance_window_label_font_size");
        labelSize.MinWidth(250); labelSize.MaxWidth(360); labelSize.HorizontalAlignment(HorizontalAlignment::Left);
        for (int32_t size = 8; size <= 14; ++size)
        {
            auto item = ComboBoxItem(); item.Content(box_value(std::to_wstring(size) + L" pt")); item.Tag(box_value(size)); labelSize.Items().Append(item);
        }
        const auto currentLabelSize = static_cast<int32_t>(State().GetNamedNumber(L"window_label_font_size", 9));
        labelSize.SelectedIndex(std::clamp(currentLabelSize, 8, 14) - 8);
        labelSize.SelectionChanged([this](auto const& sender, auto const&)
        {
            auto control = sender.as<ComboBox>();
            if (!hydrating_ && control.SelectedItem())
                SetValue(L"window_label_font_size", JsonValue::CreateNumberValue(unbox_value<int32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));
        });
        labelSizeGroup.Children().Append(labelSize); style.Children().Append(labelSizeGroup);

        // The live preview now leads the page; see the top of BuildAppearance.

        auto refreshPreview = [this]() { ScheduleWidgetPreview(); };
        auto buttons = StackPanel(); buttons.Orientation(Orientation::Horizontal); buttons.Spacing(8);
        buttons.SizeChanged([](auto const& sender, auto const&)
        {
            auto panel = sender.as<StackPanel>();
            panel.Orientation(panel.ActualWidth() < 480 ? Orientation::Vertical : Orientation::Horizontal);
        });
        for (auto [label, action] : std::array<std::pair<std::wstring, std::wstring>, 3>{{
            {L"Recommended",L"recommended"},{L"Reset appearance",L"reset_appearance"},{L"Reset colors",L"reset_colors"}}})
        {
            auto button = Button(); button.Content(box_value(label));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(button, std::wstring(L"AppearanceAction_") + action);
            button.Click([this, action](auto const&, auto const&) { Action(action); }); buttons.Children().Append(button);
        }
        style.Children().Append(buttons); styleCard.Child(style); PageHost().Children().Append(styleCard);

        auto marksCard = Card(); auto marks = StackPanel(); marks.Spacing(14);
        marks.Children().Append(Label(L"Provider marks", 19));
        auto marksNote = Label(L"Show or hide each provider’s mark and percentage in the widget. Provider marks alone do not count toward the visible usage value.", 12);
        marksNote.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        marks.Children().Append(marksNote);
        for (auto const& [markKey, markTitle] : std::array<std::pair<std::wstring, std::wstring>, 7>{{
            {L"show_claude_icon",L"Claude Code mark"},{L"show_codex_icon",L"Codex mark"},
            {L"show_antigravity_icon",L"Antigravity mark"},{L"show_opencode_icon",L"OpenCode mark"},
            {L"show_cursor_icon",L"Cursor mark"},{L"show_grok_icon",L"Grok mark"},
            {L"show_copilot_icon",L"Copilot mark"}}})
        {
            auto copy = StackPanel(); copy.Spacing(3); copy.Children().Append(Label(markTitle));
            auto markToggle = ToggleSwitch(); SetSwitchContent(markToggle);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(markToggle, markTitle);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(markToggle, std::wstring(L"Setting_") + markKey);
            markToggle.IsOn(State().GetNamedBoolean(markKey, true));
            markToggle.Toggled([this, markKey](auto const& sender, auto const&)
            {
                auto control = sender.as<ToggleSwitch>();
                if (hydrating_) return;
                SetValue(markKey, JsonValue::CreateBooleanValue(control.IsOn()));
            });
            marks.Children().Append(ToggleRow(copy, markToggle));
        }
        marksCard.Child(marks); PageHost().Children().Append(marksCard);

        auto colorsCard = Card(); auto colors = StackPanel(); colors.Spacing(12);
        colors.Children().Append(Label(L"Widget colors", 19));
        colors.Children().Append(Label(L"Progress-bar accents stay provider-specific. The shared text color applies to provider marks and usage percentages.", 12));
        colors.Children().Append(Label(L"Progress-bar accents", 14));
        for (auto const& [key, title] : std::array<std::pair<std::wstring, std::wstring>, 8>{{
            {L"claude_accent",L"Claude Code bar accent"},{L"codex_accent",L"Codex bar accent"},
            {L"antigravity_accent",L"Antigravity bar accent"},{L"opencode_accent",L"OpenCode bar accent"},
            {L"cursor_accent",L"Cursor bar accent"},{L"grok_accent",L"Grok bar accent"},
            {L"copilot_accent",L"Copilot bar accent"},
            {L"usage_text_color",L"Provider mark and percentage color"}}})
        {
            auto group = StackPanel(); group.Spacing(6); group.Children().Append(Label(title));
            auto row = StackPanel(); row.Orientation(Orientation::Horizontal); row.Spacing(8);
            row.SizeChanged([](auto const& sender, auto const&)
            {
                auto panel = sender.as<StackPanel>();
                panel.Orientation(panel.ActualWidth() < 610 ? Orientation::Vertical : Orientation::Horizontal);
            });
            auto input = TextBox();
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(input, title);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(input, std::wstring(L"Color_") + key);
            input.PlaceholderText(L"Default color"); input.Width(145);
            const auto savedText = StoredColorText(State(), key);
            const auto& draft = key == L"claude_accent" ? claudeColorDraft_ : key == L"codex_accent" ? codexColorDraft_ : key == L"antigravity_accent" ? antigravityColorDraft_ : key == L"opencode_accent" ? opencodeColorDraft_ : key == L"cursor_accent" ? cursorColorDraft_ : key == L"grok_accent" ? grokColorDraft_ : key == L"copilot_accent" ? copilotColorDraft_ : usageTextColorDraft_;
            input.Text(draft ? hstring(*draft) : hstring(savedText));
            input.TextChanging([this, key, refreshPreview](auto const& sender, auto const&)
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
                SetColorDraft(key, text); refreshPreview();
            });

            auto pickerButton = Button(); pickerButton.Content(box_value(L"Choose color…"));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(pickerButton, std::wstring(L"ChooseColor_") + key);
            auto flyout = Flyout(); auto pickerPanel = StackPanel(); pickerPanel.Spacing(10); pickerPanel.Padding(Thickness{12,12,12,12});
            auto picker = ColorPicker(); picker.IsAlphaEnabled(false); picker.IsColorChannelTextInputVisible(true);
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(picker, title + L" RGB color");
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(picker, std::wstring(L"ColorPicker_") + key);
            pickerPanel.Children().Append(picker);
            auto pickerActions = StackPanel(); pickerActions.Orientation(Orientation::Horizontal); pickerActions.Spacing(8);
            auto useColor = Button(); useColor.Content(box_value(L"Use color"));
            auto cancelColor = Button(); cancelColor.Content(box_value(L"Cancel"));
            pickerActions.Children().Append(useColor); pickerActions.Children().Append(cancelColor); pickerPanel.Children().Append(pickerActions);
            flyout.Content(pickerPanel); pickerButton.Flyout(flyout);
            flyout.Opened([this, picker, input, key, theme](auto const&, auto const&)
            {
                auto color = ParseHexColor(std::wstring(input.Text().c_str()));
                if (!color) color = ParseHexColor(StoredColorText(State(), key));
                const auto effective = state_ ? state_.GetNamedObject(L"effective_appearance", JsonObject()) : JsonObject();
                const bool systemDark = effective.GetNamedBoolean(L"dark", true);
                const auto palette = theme.SelectedItem() ? std::wstring(unbox_value<hstring>(theme.SelectedItem().as<ComboBoxItem>().Tag()).c_str()) : Text(L"palette");
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
            useColor.Click([this, picker, input, key, flyout, refreshPreview](auto const&, auto const&)
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
                SetColorDraft(key, value); refreshPreview(); flyout.Hide();
            });
            cancelColor.Click([flyout](auto const&, auto const&) { flyout.Hide(); });

            auto apply = Button(); apply.Content(box_value(L"Apply color"));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(apply, std::wstring(L"ApplyColor_") + key);
            apply.Click([this, input, key](auto const&, auto const&)
            {
                const std::wstring raw(input.Text().c_str()); const auto color = ParseHexColor(raw);
                if (!color) { SetColorDraft(key, raw); ShowMessage(L"Enter exactly # followed by six ASCII hexadecimal digits, for example #D97757.", true); return; }
                const auto normalized = FormatHexColor(*color);
                syncingColorText_ = true; input.Text(normalized); syncingColorText_ = false;
                SetColorDraft(key, normalized);
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
            auto discard = Button(); discard.Content(box_value(L"Discard"));
            Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(discard, std::wstring(L"DiscardColor_") + key);
            discard.Click([this, input, key, refreshPreview](auto const&, auto const&)
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
                hydrating_ = wasHydrating; refreshPreview(); ShowMessage(L"Color draft discarded."); RefreshSettingsControlsIfSafe();
            });
            row.Children().Append(input); row.Children().Append(pickerButton); row.Children().Append(apply); row.Children().Append(discard);
            group.Children().Append(row); colors.Children().Append(group);
        }
        colorsCard.Child(colors); PageHost().Children().Append(colorsCard);
        if (pageScroll_ && pageScroll_.VerticalOffset() > 8)
        {
            auto weak = get_weak();
            dispatcher_.TryEnqueue([weak]()
            {
                if (auto self = weak.get()) self->UpdateStickyPreview();
            });
        }
    }

    void MainWindow::ClearStickyPreview()
    {
        appearancePreviewFloating_ = false;
        appearancePreviewFloatOffset_ = 0;
        if (appearancePreviewCard_)
        {
            uint32_t index{};
            if (StickyPreviewHost().Children().IndexOf(appearancePreviewCard_, index))
                StickyPreviewHost().Children().RemoveAt(index);
            appearancePreviewCard_ = nullptr;
        }
        if (appearancePreviewPlaceholder_)
        {
            uint32_t placeholderIndex{};
            if (PageHost().Children().IndexOf(appearancePreviewPlaceholder_, placeholderIndex))
                PageHost().Children().RemoveAt(placeholderIndex);
            appearancePreviewPlaceholder_ = nullptr;
        }
        StickyPreviewHost().Visibility(Visibility::Collapsed);
    }

    void MainWindow::UpdateStickyPreview()
    {
        if (closed_ || page_ != L"Appearance" || !appearancePreviewCard_ || !pageScroll_) return;
        try
        {
            if (!appearancePreviewFloating_)
            {
                if (pageScroll_.VerticalOffset() <= 8) return;
                Windows::Foundation::Point origin{ 0, 0 };
                const auto top = appearancePreviewCard_.TransformToVisual(pageScroll_).TransformPoint(origin).Y;
                if (top > 0) return;
                uint32_t index{};
                if (!PageHost().Children().IndexOf(appearancePreviewCard_, index)) return;
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
            else if (pageScroll_.VerticalOffset() <= appearancePreviewFloatOffset_ - 32)
            {
                uint32_t index{};
                if (StickyPreviewHost().Children().IndexOf(appearancePreviewCard_, index))
                    StickyPreviewHost().Children().RemoveAt(index);
                if (appearancePreviewPlaceholder_)
                {
                    uint32_t placeholderIndex{};
                    if (PageHost().Children().IndexOf(appearancePreviewPlaceholder_, placeholderIndex))
                        PageHost().Children().RemoveAt(placeholderIndex);
                    appearancePreviewPlaceholder_ = nullptr;
                }
                PageHost().Children().InsertAt(0, appearancePreviewCard_);
                appearancePreviewCard_.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
                StickyPreviewHost().Visibility(Visibility::Collapsed);
                appearancePreviewFloating_ = false;
                appearancePreviewFloatOffset_ = 0;
            }
        }
        catch (...) {}
    }

    void MainWindow::BuildPlacement()
    {
        auto card=Card();auto stack=StackPanel();stack.Spacing(16);stack.Children().Append(Label(L"Taskbar placement",19));stack.Children().Append(Label(L"Choose the display where the widget should appear. The widget stays aligned with the taskbar.",13));auto monitors=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(monitors,L"Monitor device");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(monitors,L"MonitorSelector");monitors.MinWidth(250);monitors.MaxWidth(520);monitors.HorizontalAlignment(HorizontalAlignment::Left);auto automatic=ComboBoxItem();automatic.Content(box_value(L"Automatic (primary display)"));automatic.Tag(box_value(L""));monitors.Items().Append(automatic);if(state_&&state_.HasKey(L"monitors"))for(auto const& v:state_.GetNamedArray(L"monitors")){auto m=v.GetObject();auto device=m.GetNamedString(L"device",L"");auto item=ComboBoxItem();item.Content(box_value(std::wstring(m.GetNamedBoolean(L"primary",false)?L"Primary display · ":L"Display · ")+device));item.Tag(box_value(device));monitors.Items().Append(item);}monitors.SelectedIndex(0);auto current=Text(L"monitor_device");if(state_&&State().HasKey(L"monitor_device")&&State().GetNamedValue(L"monitor_device").ValueType()==JsonValueType::String)current=std::wstring(State().GetNamedString(L"monitor_device").c_str());for(uint32_t i=0;i<monitors.Items().Size();++i)if(std::wstring(unbox_value<hstring>(monitors.Items().GetAt(i).as<ComboBoxItem>().Tag()).c_str())==current)monitors.SelectedIndex(i);monitors.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem()){auto val=unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag());SetValue(L"monitor_device",val.empty()?JsonValue::CreateNullValue():JsonValue::CreateStringValue(val));}});stack.Children().Append(monitors);
         auto sideCopy=StackPanel();sideCopy.Spacing(3);sideCopy.Children().Append(Label(L"Anchor to the left"));auto sideDetail=Label(L"Place the widget at the left edge of the taskbar.",12);sideDetail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());sideCopy.Children().Append(sideDetail);auto side=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(side,L"Anchor to the left");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(side,L"Setting_anchor_left");SetSwitchContent(side);side.IsOn(Bool(L"anchor_left"));side.Toggled([this](auto const&sender,auto const&){auto control=sender.as<ToggleSwitch>();SetValue(L"anchor_left",JsonValue::CreateBooleanValue(control.IsOn()));});stack.Children().Append(ToggleRow(sideCopy,side));auto reset=Button();reset.Content(box_value(L"Reset position"));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(reset,L"ResetPlacement");reset.HorizontalAlignment(HorizontalAlignment::Left);reset.Click([this](auto const&,auto const&){Action(L"reset_position");});stack.Children().Append(reset);card.Child(stack);PageHost().Children().Append(card);
    }

    void MainWindow::BuildAbout()
    {
        auto hero = Card();
        auto h = StackPanel(); h.Spacing(15);
        auto nameRow = StackPanel(); nameRow.Orientation(Orientation::Horizontal); nameRow.Spacing(16); nameRow.VerticalAlignment(VerticalAlignment::Center);
        auto appIcon = Image(); appIcon.Width(64); appIcon.Height(64); appIcon.Stretch(Media::Stretch::Uniform); appIcon.VerticalAlignment(VerticalAlignment::Center);
        SetImageFromResource(appIcon, IDR_ABOUT_ICON);
        auto appName = Label(L"Codex Usage", 28); appName.VerticalAlignment(VerticalAlignment::Center);
        appName.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
        nameRow.Children().Append(appIcon); nameRow.Children().Append(appName);
        h.Children().Append(nameRow);
        h.Children().Append(Label(L"A lightweight, portable way to keep an eye on Codex and Claude Code usage from your taskbar.", 15));
        auto roseLogo = Image(); roseLogo.Width(128); roseLogo.Height(40); roseLogo.Stretch(Media::Stretch::Uniform); roseLogo.HorizontalAlignment(HorizontalAlignment::Left);
        SetImageFromResource(roseLogo, IDR_DATAROSE_LOGO);
        auto developerLink = HyperlinkButton(); developerLink.Content(roseLogo); developerLink.NavigateUri(Windows::Foundation::Uri(L"https://datarose.net"));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(developerLink, L"Visit Datarose website");
        developerLink.HorizontalAlignment(HorizontalAlignment::Left);
        auto footer = StackPanel(); footer.Orientation(Orientation::Horizontal); footer.Spacing(18); footer.VerticalAlignment(VerticalAlignment::Center);
        footer.Children().Append(developerLink);
        versionText_ = Label(state_ ? std::wstring(std::wstring(L"Version ") + state_.GetNamedString(L"version", L"unknown").c_str()) : std::wstring(L"Connecting to the widget…"), 13);
        versionText_.VerticalAlignment(VerticalAlignment::Center);
        versionText_.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        footer.Children().Append(versionText_); h.Children().Append(footer);
        hero.Child(h); PageHost().Children().Append(hero);

        auto how = Card(); auto howStack = StackPanel(); howStack.Spacing(10);
        howStack.Children().Append(Label(L"Designed to stay local", 19));
        howStack.Children().Append(Label(L"Codex Usage runs locally and reuses sign-ins from the Codex CLI and Claude Code CLI already installed on your PC. There is no separate Codex Usage account, login, or API-key setup.", 14));
        howStack.Children().Append(Label(L"It reads quota and reset information from provider services and shows the available usage windows in your taskbar. Quota requests go directly from this app to those services; they do not pass through a Codex Usage server.", 14));
        how.Child(howStack); PageHost().Children().Append(how);

        auto info = Card(); auto s = StackPanel(); s.Spacing(12);
        s.Children().Append(Label(L"Project & licensing", 19));
        auto projectRow = StackPanel(); projectRow.Orientation(Orientation::Horizontal); projectRow.Spacing(12); projectRow.VerticalAlignment(VerticalAlignment::Center);
        auto repo = Button(); repo.Content(box_value(L"View project")); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(repo, L"ViewProject"); repo.HorizontalAlignment(HorizontalAlignment::Left); repo.Click([this](auto const&, auto const&) { Action(L"repository"); });
        auto license = Label(L"Licensed under AGPL-3.0-or-later", 14);
        license.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        projectRow.Children().Append(repo); projectRow.Children().Append(license); s.Children().Append(projectRow);
        s.Children().Append(Label(L"Third-party license terms and required notices are included with this application.", 14));
        auto noticesButton = Button(); noticesButton.Content(box_value(L"Third-party licenses and notices"));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(noticesButton, L"ThirdPartyNotices");
        noticesButton.HorizontalAlignment(HorizontalAlignment::Left);
        noticesButton.Click([this](auto const&, auto const&) { OpenThirdPartyNotices(); });
        s.Children().Append(noticesButton);
        info.Child(s); PageHost().Children().Append(info);

        auto updates = Card(); auto updatesStack = StackPanel(); updatesStack.Spacing(10);
        updatesStack.Children().Append(Label(L"Software updates", 19));
        updateStatusText_ = Label(state_ ? std::wstring(state_.GetNamedString(L"update_status", L"Update status unavailable").c_str()) : std::wstring(L"Checking update status…"), 14);
        updateStatusText_.TextWrapping(TextWrapping::Wrap);
        updatesStack.Children().Append(updateStatusText_);
        checkForUpdatesButton_ = Button();
        const bool updateAvailable = state_ && state_.GetNamedBoolean(L"update_available", false);
        const bool updateBusy = state_ && state_.GetNamedBoolean(L"update_busy", false);
        checkForUpdatesButton_.Content(box_value(updateBusy ? L"Working…" : updateAvailable ? L"Install update" : L"Check for updates"));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(checkForUpdatesButton_, L"CheckForUpdates");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(checkForUpdatesButton_,
            updateBusy ? L"Update in progress" : updateAvailable ? L"Install available update" : L"Check for updates");
        checkForUpdatesButton_.Style(Application::Current().Resources().Lookup(box_value(L"AccentButtonStyle")).as<Style>());
        checkForUpdatesButton_.IsEnabled(!updateBusy);
        checkForUpdatesButton_.HorizontalAlignment(HorizontalAlignment::Left);
        checkForUpdatesButton_.Click([this](auto const&, auto const&) { Action(L"update"); });
        updatesStack.Children().Append(checkForUpdatesButton_);
        updates.Child(updatesStack); PageHost().Children().Append(updates);

        auto privacy = Card();
        auto privacyStack = StackPanel(); privacyStack.Spacing(14);
        privacyStack.Children().Append(Label(L"Privacy and Security", 19));
        privacyStack.Children().Append(Label(L"A brief overview of the data Codex Usage uses to display provider quotas and maintain your settings.", 13));

        auto addPrivacySection = [&privacyStack](std::wstring const& title, std::vector<std::wstring> const& items)
        {
            auto section = StackPanel(); section.Spacing(5);
            auto heading = Label(title, 14);
            heading.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
            section.Children().Append(heading);
            for (auto const& item : items) section.Children().Append(Label(std::wstring(L"•  ") + item, 13));
            privacyStack.Children().Append(section);
        };

        addPrivacySection(L"Sign-in data", {
            L"The app uses existing sign-ins for Claude Code, Codex, and Antigravity to retrieve quota information.",
            L"Claude Code credentials may be read from Windows or WSL; Antigravity credentials are read from Windows Credential Manager."
        });
        addPrivacySection(L"Network requests", {
            L"Quota requests go directly to the enabled providers; they do not pass through a Codex Usage service.",
            L"Update checks and portable update downloads contact GitHub."
        });
        addPrivacySection(L"Local settings", {
            L"Provider selection, widget placement, appearance, polling options, and the last update-check time are stored locally."
        });
        addPrivacySection(L"Account setup", {
            L"No Codex Usage account or separate API-key setup is required."
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
        uninstallStack.Children().Append(Label(L"Uninstall Codex Usage", 19));
        uninstallStack.Children().Append(Label(L"This removes the app and its local files. This action cannot be undone.", 14));
        uninstallDescription_ = Label(channel == L"winget"
            ? L"WinGet will open its uninstall flow and ask you to confirm."
            : L"The built-in flow will ask you to confirm before removing anything.", 13);
        uninstallDescription_.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        uninstallStack.Children().Append(uninstallDescription_);

        auto uninstall = Button();
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(uninstall, L"UninstallApp");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(uninstall, L"Uninstall Codex Usage");
        uninstall.Style(Navigation().Resources().Lookup(box_value(L"UninstallButtonStyle")).as<Style>());
        uninstall.HorizontalAlignment(HorizontalAlignment::Left);
        auto uninstallContent = StackPanel();
        uninstallContent.Orientation(Orientation::Horizontal); uninstallContent.Spacing(8);
        auto uninstallIcon = FontIcon(); uninstallIcon.Glyph(L"\uE74D"); uninstallIcon.FontSize(14);
        uninstallContent.Children().Append(uninstallIcon);
        uninstallContent.Children().Append(Label(L"Uninstall Codex Usage…", 14));
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
        updateStatusText_ = Label(state_ ? std::wstring(state_.GetNamedString(L"update_status", L"Update status unavailable").c_str()) : std::wstring(L"Checking update status…"), 14);
        updateStatusText_.TextWrapping(TextWrapping::Wrap);
        stack.Children().Append(updateStatusText_);
        auto version = Label(state_ ? std::wstring(L"Installed version  ·  ") + state_.GetNamedString(L"version", L"unknown").c_str() : L"Installed version is being checked…", 13);
        version.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        stack.Children().Append(version);
        checkForUpdatesButton_ = Button();
        checkForUpdatesButton_.Content(box_value(available ? L"Install update" : L"Check for updates"));
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(checkForUpdatesButton_, L"CheckForUpdates");
        Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(checkForUpdatesButton_, available ? L"Install available update" : L"Check for updates");
        checkForUpdatesButton_.Style(Application::Current().Resources().Lookup(box_value(L"AccentButtonStyle")).as<Style>());
        checkForUpdatesButton_.IsEnabled(!(state_ && state_.GetNamedBoolean(L"update_busy", false)));
        checkForUpdatesButton_.HorizontalAlignment(HorizontalAlignment::Left);
        checkForUpdatesButton_.Click([this](auto const&, auto const&) { Action(L"update"); });
        stack.Children().Append(checkForUpdatesButton_);
        card.Child(stack); PageHost().Children().Append(card);
    }
}
