#include "pch.h"
#include "MainWindow.xaml.h"
#include "MainWindow.g.cpp"
#include "MainWindow.xaml.g.hpp"
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
    Border Card()
    {
        auto border = Border(); border.Background(Application::Current().Resources().Lookup(box_value(L"CardBackgroundFillColorDefaultBrush")).as<Media::Brush>());
        border.BorderBrush(Application::Current().Resources().Lookup(box_value(L"CardStrokeColorDefaultBrush")).as<Media::Brush>());
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
        debounceTimer_ = dispatcher_.CreateTimer();
        debounceTimer_.Interval(std::chrono::seconds(4));
        debounceTimer_.IsRepeating(false);
        debounceTimer_.Tick([this](auto const&, auto const&) { debounceElapsed_ = true; SendNextMutation(); });
        messageTimer_ = dispatcher_.CreateTimer();
        messageTimer_.Interval(std::chrono::seconds(10));
        messageTimer_.IsRepeating(false);
        messageTimer_.Tick([this](auto const&, auto const&) { MessageBar().IsOpen(false); });
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
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd_));
        closingToken_ = AppWindow().Closing([this](auto const& sender, auto const& args) { OnWindowClosing(sender, args); });
        MONITORINFO monitor{ sizeof(monitor) };
        if (GetMonitorInfoW(MonitorFromWindow(hwnd_, MONITOR_DEFAULTTONEAREST), &monitor))
        {
            const auto dpi = GetDpiForWindow(hwnd_);
            const auto width = std::min<int>(MulDiv(1080, dpi, 96), monitor.rcWork.right - monitor.rcWork.left);
            const auto height = std::min<int>(MulDiv(760, dpi, 96), monitor.rcWork.bottom - monitor.rcWork.top);
            AppWindow().MoveAndResize({ monitor.rcWork.left + (monitor.rcWork.right - monitor.rcWork.left - width) / 2,
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
        if (messageTimer_) messageTimer_.Stop();
        if (debounceTimer_) debounceTimer_.Stop();
        stopping_.store(true); requestsChanged_.notify_all();
        if (pipeHandle_ != INVALID_HANDLE_VALUE) CancelIoEx(pipeHandle_, nullptr);
        if (ioThread_.joinable()) CancelSynchronousIo(ioThread_.native_handle());
        if (ioThread_.joinable()) ioThread_.join();
    }

    void MainWindow::InitializePipe()
    {
        auto get = Request(L"get"); inFlight_ = nextId_++; initialGetPending_ = true;
        get.Insert(L"id", JsonValue::CreateNumberValue(inFlight_));
        const auto initialRequest = Utf8(get.Stringify().c_str());
        ioThread_ = std::thread([this, initialRequest]()
        {
            try { winrt::init_apartment(winrt::apartment_type::multi_threaded); }
            catch (...)
            {
                stopping_ = true;
                dispatcher_.TryEnqueue([this]() { ShowMessage(L"The settings connection could not initialize.", true); });
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
                dispatcher_.TryEnqueue([this]() { ShowMessage(L"Could not connect to Codex Usage. Close this window and try again from the widget.", true); }); return;
            }
            ULONG serverPid = 0;
            if (!GetNamedPipeServerProcessId(pipe, &serverPid) || !parentPid_ || serverPid != parentPid_)
            {
                CloseHandle(pipe);
                dispatcher_.TryEnqueue([this]() { ShowMessage(L"The settings connection could not be verified. Reopen settings from the widget.", true); });
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
                CloseHandle(pipe);
                dispatcher_.TryEnqueue([this]() { ShowMessage(L"The first settings request could not be sent. Reopen settings from the widget.", true); });
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
                if (length == 0 || length > 65536) break;
                std::string body(length, '\0');
                if (!readExact(body.data(), length)) break;
                try
                {
                    auto message = JsonObject::Parse(Wide(body));
                    dispatcher_.TryEnqueue([this, message]() { Receive(message); });
                }
                catch (...) { connected = false; }
            }
            stopping_.store(true); requestsChanged_.notify_all(); CancelIoEx(pipe, nullptr); writer.join();
            CloseHandle(pipe); pipeHandle_ = INVALID_HANDLE_VALUE;
            dispatcher_.TryEnqueue([this]() { if (m_inner) Close(); });
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
        if (debounceTimer_) debounceTimer_.Stop();
        debounceElapsed_ = true;
        flushRequested_ = true;
        SendNextMutation();
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
                if ((!flightChanges_ || !flightChanges_.HasKey(key)) && server.HasKey(key) && server.GetNamedValue(key).Stringify() == pair.Value().Stringify()) continue;
                if (changes.Size() == 20) { remainder.Insert(key, pair.Value()); continue; }
                auto change = JsonObject(); change.Insert(L"key", JsonValue::CreateStringValue(key)); change.Insert(L"value", pair.Value()); changes.Append(change);
                snapshot.Insert(key, pair.Value());
            }
            pendingChanges_ = remainder;
            if (changes.Size() != 0)
            {
                auto request = Request(L"set_batch"); request.Insert(L"changes", changes);
                flightChanges_ = snapshot; mutationInFlight_ = true;
                ShowMessage(L"Applying changes…");
                Send(request);
                return;
            }
        }
        if (persistNeeded_ && flushRequested_)
        {
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
        if (closeRequested_)
        {
            if (persistNeeded_ || HasPendingChanges() || inFlight_ != 0) return;
            closeRequested_ = false;
            closingFromParent_ = true;
            Close();
            return;
        }
        if (deferredAction_ && !persistNeeded_ && !HasPendingChanges() && inFlight_ == 0)
        {
            auto action = *deferredAction_; deferredAction_.reset();
            settingsActionInFlight_ = action == L"recommended" || action == L"reset_appearance" || action == L"reset_colors";
            auto request = Request(L"action"); request.Insert(L"action", JsonValue::CreateStringValue(action)); Send(request);
        }
    }

    void MainWindow::OnWindowClosing(Microsoft::UI::Windowing::AppWindow const&, Microsoft::UI::Windowing::AppWindowClosingEventArgs const& args)
    {
        if (closingFromParent_ || stopping_ || (!HasPendingChanges() && !mutationInFlight_ && !persistNeeded_)) return;
        args.Cancel(true); closeRequested_ = true; FlushPending();
    }

    void MainWindow::Receive(JsonObject const& message)
    {
        if (message.HasKey(L"event"))
        {
            auto event = message.GetNamedString(L"event", L"");
            if (event == L"shutdown") { stopping_ = true; closingFromParent_ = true; Close(); return; }
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
                }
                UpdateClaudeAvailability();
                UpdateAboutControls();
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
        const bool wasMutation = mutationInFlight_;
        const bool wasSettingsAction = settingsActionInFlight_;
        initialGetPending_ = false;
        settingsActionInFlight_ = false;
        inFlight_ = 0;
        const auto responseRevision = static_cast<uint64_t>(message.GetNamedNumber(L"revision", 0));
        if (message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object && responseRevision >= revision_)
        { state_ = message.GetNamedObject(L"state"); revision_ = responseRevision; }
        if (isInitialGet)
        {
            const auto settings = state_ ? state_.GetNamedObject(L"settings", JsonObject()) : JsonObject();
            hydrating_ = true;
            Navigation().IsPaneOpen(settings.GetNamedBoolean(L"settings_pane_open", true));
            hydrating_ = false;
            paneStateKnown_ = true;
        }
        UpdateClaudeAvailability();
        UpdateAboutControls();
        const bool ok = message.GetNamedBoolean(L"ok", false);
        const bool wasPersist = persistInFlight_;
        persistInFlight_ = false;
        if (mutationInFlight_)
        {
            mutationInFlight_ = false;
            if (!ok && message.GetNamedString(L"error_code", L"") == L"save_failed")
            {
                persistNeeded_ = true;
                ShowMessage(std::wstring(message.GetNamedString(L"error", L"Changes are active, but could not be saved.").c_str()), true);
            }
            else if (!ok)
            {
                // A rejected batch was not applied. Do not retry it automatically;
                // only edits made after it was sent remain eligible for a new batch.
                flushRequested_ = false; debounceElapsed_ = false;
                deferredAction_.reset(); closeRequested_ = false;
                ShowMessage(std::wstring(message.GetNamedString(L"error", L"The setting could not be applied.").c_str()), true);
                if (HasPendingChanges() && debounceTimer_) { debounceTimer_.Stop(); debounceTimer_.Start(); }
            }
            else
            {
                persistNeeded_ = false;
                if (!isInitialGet) ShowMessage(L"Saved. Changes are now in effect.");
            }
            flightChanges_ = nullptr;
        }
        else if (wasPersist)
        {
            if (ok) { persistNeeded_ = false; ShowMessage(L"Changes saved."); }
            else { persistNeeded_ = true; flushRequested_ = false; ShowMessage(std::wstring(message.GetNamedString(L"error", L"Changes could not be saved. Try again.").c_str()), true); }
        }
        else if (!ok) ShowMessage(std::wstring(message.GetNamedString(L"error", L"The request could not be completed.").c_str()), true);
        if (isInitialGet || (wasMutation && !ok))
        {
            Hydrate();
            settingsRefreshPending_ = false;
        }
        else if (wasSettingsAction && ok)
        {
            settingsRefreshPending_ = true;
        }
        if (HasPendingChanges() && !flushRequested_ && debounceElapsed_) SendNextMutation();
        else if (flushRequested_) SendNextMutation();
        else ContinueDeferredWork();
        RefreshSettingsControlsIfSafe();
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
        debounceElapsed_ = false;
        flushRequested_ = false;
        if (debounceTimer_) { debounceTimer_.Stop(); debounceTimer_.Start(); }
        ShowMessage(L"Changes will be applied shortly.");
    }
    void MainWindow::Action(std::wstring const& action)
    {
        deferredAction_ = action;
        FlushPending();
    }
    void MainWindow::ShowMessage(std::wstring const& text, bool error)
    {
        MessageBar().Message(text);
        MessageBar().Severity(error ? InfoBarSeverity::Error : InfoBarSeverity::Informational);
        MessageBar().IsOpen(true);
        if (messageTimer_) { messageTimer_.Stop(); messageTimer_.Start(); }
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
        if (checkForUpdatesButton_)
            checkForUpdatesButton_.IsEnabled(!(state_ && state_.GetNamedBoolean(L"update_busy", false)));
        if (uninstallDescription_)
        {
            const auto channel = state_ ? state_.GetNamedString(L"install_channel", L"portable") : hstring(L"portable");
            uninstallDescription_.Text(channel == L"winget"
                ? L"This permanently removes the app. WinGet will open its uninstall flow, which asks you to confirm before anything is removed."
                : L"This permanently removes Codex Usage from this PC. The built-in flow asks you to confirm before anything is removed.");
        }
    }
    void MainWindow::Hydrate() { RenderPage(page_); }

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
        page_ = nextPage; RenderPage(page_);
    }
    void MainWindow::RenderPage(std::wstring const& page)
    {
        appearanceColorDraft_ = false;
        claudeColorDraft_ = false;
        codexColorDraft_ = false;
        page_ = page; PageHost().Children().Clear();
        claudeToggle_ = nullptr;
        versionText_ = nullptr;
        updateStatusText_ = nullptr;
        uninstallDescription_ = nullptr;
        checkForUpdatesButton_ = nullptr;
        PageTitle().Text(page); PageEyebrow().Text(page == L"About" ? L"YOUR APP" : L"PREFERENCES");
        const std::map<std::wstring, std::wstring> summaries{
            {L"General",L"Manage startup, taskbar visibility, and language."}, {L"Monitoring",L"Choose providers and refresh options."},
            {L"Appearance",L"Choose the widget’s theme and visual settings."}, {L"Placement",L"Choose the display and taskbar position."},
            {L"About",L"A clear view of your usage, right where you need it."}};
        PageSummary().Text(summaries.at(page));
        hydrating_ = true;
        if (page == L"General") BuildGeneral(); else if (page == L"Monitoring") BuildMonitoring(); else if (page == L"Appearance") BuildAppearance(); else if (page == L"Placement") BuildPlacement(); else BuildAbout();
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
            auto sw = ToggleSwitch(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw, title); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(sw, std::wstring(L"Setting_") + key); SetSwitchContent(sw); sw.IsOn(Bool(key)); sw.Toggled([this, key](auto const& sender, auto const&) { SetValue(key, JsonValue::CreateBooleanValue(sender.as<ToggleSwitch>().IsOn())); });
            stack.Children().Append(ToggleRow(copy, sw));
        };
        toggle(L"startup", L"Start with Windows", L"Open the widget when you sign in.");
        toggle(L"widget_visible", L"Show the taskbar widget", L"Keep the live usage view visible in your taskbar.");
        toggle(L"show_tray_icon", L"Show the Windows notification-area icon", L"Show or hide Codex Usage’s separate icon beside the Windows clock.");
        toggle(L"show_provider_icons", L"Show provider marks inside the widget", L"Show or hide the small provider icons in the usage widget; this does not affect the Windows notification-area icon.");
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
        auto card=Card();auto stack=StackPanel();stack.Spacing(16);stack.Children().Append(Label(L"Widget style",19));
         auto addCombo=[this,&stack](std::wstring title,std::wstring key,std::vector<std::pair<std::wstring,std::wstring>> values){auto group=StackPanel();group.Spacing(6);group.Children().Append(Label(title));auto combo=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(combo,title);Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(combo,std::wstring(L"Appearance_")+key);combo.MinWidth(250);combo.MaxWidth(520);combo.HorizontalAlignment(HorizontalAlignment::Stretch);for(auto const& [value,label]:values){auto item=ComboBoxItem();item.Content(box_value(label));item.Tag(box_value(value));combo.Items().Append(item);}auto current=Text(key);for(uint32_t i=0;i<combo.Items().Size();++i)if(unbox_value<hstring>(combo.Items().GetAt(i).as<ComboBoxItem>().Tag())==current)combo.SelectedIndex(i);combo.SelectionChanged([this,key](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(key,JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag())));});group.Children().Append(combo);stack.Children().Append(group);};
        addCombo(L"Theme",L"palette",{{L"system",L"Follow Windows"},{L"high_contrast_dark",L"Dark"},{L"high_contrast_light",L"Light"}});addCombo(L"Progress bars",L"bar_style",{{L"continuous",L"Continuous"},{L"segmented",L"Segmented"}});addCombo(L"Bar thickness",L"bar_thickness",{{L"standard",L"Standard"},{L"slim",L"Slim"}});addCombo(L"Text size",L"font_size",{{L"standard",L"Standard"},{L"large",L"Large"}});
        auto buttons=StackPanel();buttons.Spacing(8);for(auto [label,action]:std::array<std::pair<std::wstring,std::wstring>,3>{{{L"Recommended",L"recommended"},{L"Reset appearance",L"reset_appearance"},{L"Reset colors",L"reset_colors"}}}){auto b=Button();b.Content(box_value(label));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(b,std::wstring(L"AppearanceAction_")+action);b.HorizontalAlignment(HorizontalAlignment::Left);b.Click([this,action](auto const&,auto const&){Action(action);});buttons.Children().Append(b);}stack.Children().Append(buttons);
        card.Child(stack);PageHost().Children().Append(card);
        auto colors=Card();auto c=StackPanel();c.Spacing(12);c.Children().Append(Label(L"Provider colors",19));c.Children().Append(Label(L"Use the widget’s recommended colors, or set a custom RGB accent from hex." ,12));
        for(auto const& [key,title]:std::array<std::pair<std::wstring,std::wstring>,2>{{{L"claude_accent",L"Claude Code accent"},{L"codex_accent",L"Codex accent"}}}){auto group=StackPanel();group.Spacing(6);group.Children().Append(Label(title));auto row=StackPanel();row.Orientation(Orientation::Horizontal);row.Spacing(8);auto input=TextBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(input,title);Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(input,std::wstring(L"Color_")+key);input.PlaceholderText(L"Default color");input.Width(145);auto val=State().GetNamedValue(key,JsonValue::CreateNullValue());if(val.ValueType()==JsonValueType::Array){auto a=val.GetArray();if(a.Size()==3){std::wostringstream hex;hex<<L"#"<<std::uppercase<<std::hex<<std::setfill(L'0');for(auto const& n:a)hex<<std::setw(2)<<static_cast<int>(n.GetNumber());input.Text(hex.str());}}input.TextChanged([this,key](auto const& sender,auto const&){const bool dirty=!sender.as<TextBox>().Text().empty();if(key==L"claude_accent")claudeColorDraft_=dirty;else codexColorDraft_=dirty;appearanceColorDraft_=claudeColorDraft_||codexColorDraft_;});auto set=Button();set.Content(box_value(L"Apply color"));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(set,std::wstring(L"ApplyColor_")+key);set.Click([this,input,key] (auto const&,auto const&){auto text=input.Text().c_str();unsigned r,g,b;if(swscanf_s(text,L"#%2x%2x%2x",&r,&g,&b)!=3){ShowMessage(L"Enter a color like #D97757.",true);return;}auto arr=JsonArray();arr.Append(JsonValue::CreateNumberValue(r));arr.Append(JsonValue::CreateNumberValue(g));arr.Append(JsonValue::CreateNumberValue(b));if(key==L"claude_accent")claudeColorDraft_=false;else codexColorDraft_=false;appearanceColorDraft_=claudeColorDraft_||codexColorDraft_;SetValue(key,arr);});row.Children().Append(input);row.Children().Append(set);group.Children().Append(row);c.Children().Append(group);}
        colors.Child(c);PageHost().Children().Append(colors);
    }

    void MainWindow::BuildPlacement()
    {
        auto card=Card();auto stack=StackPanel();stack.Spacing(16);stack.Children().Append(Label(L"Taskbar placement",19));stack.Children().Append(Label(L"Choose the display where the widget should appear. The widget stays aligned with the taskbar.",13));auto monitors=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(monitors,L"Monitor device");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(monitors,L"MonitorSelector");monitors.MaxWidth(520);monitors.HorizontalAlignment(HorizontalAlignment::Stretch);auto automatic=ComboBoxItem();automatic.Content(box_value(L"Automatic (primary display)"));automatic.Tag(box_value(L""));monitors.Items().Append(automatic);if(state_&&state_.HasKey(L"monitors"))for(auto const& v:state_.GetNamedArray(L"monitors")){auto m=v.GetObject();auto device=m.GetNamedString(L"device",L"");auto item=ComboBoxItem();item.Content(box_value(std::wstring(m.GetNamedBoolean(L"primary",false)?L"Primary display · ":L"Display · ")+device));item.Tag(box_value(device));monitors.Items().Append(item);}monitors.SelectedIndex(0);auto current=Text(L"monitor_device");if(state_&&State().HasKey(L"monitor_device")&&State().GetNamedValue(L"monitor_device").ValueType()==JsonValueType::String)current=std::wstring(State().GetNamedString(L"monitor_device").c_str());for(uint32_t i=0;i<monitors.Items().Size();++i)if(std::wstring(unbox_value<hstring>(monitors.Items().GetAt(i).as<ComboBoxItem>().Tag()).c_str())==current)monitors.SelectedIndex(i);monitors.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem()){auto val=unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag());SetValue(L"monitor_device",val.empty()?JsonValue::CreateNullValue():JsonValue::CreateStringValue(val));}});stack.Children().Append(monitors);
         auto sideCopy=StackPanel();sideCopy.Spacing(3);sideCopy.Children().Append(Label(L"Anchor to the left"));auto sideDetail=Label(L"Place the widget at the left edge of the taskbar.",12);sideDetail.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());sideCopy.Children().Append(sideDetail);auto side=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(side,L"Anchor to the left");Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(side,L"Setting_anchor_left");SetSwitchContent(side);side.IsOn(Bool(L"anchor_left"));side.Toggled([this](auto const&sender,auto const&){auto control=sender.as<ToggleSwitch>();SetValue(L"anchor_left",JsonValue::CreateBooleanValue(control.IsOn()));});stack.Children().Append(ToggleRow(sideCopy,side));auto reset=Button();reset.Content(box_value(L"Reset position"));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(reset,L"ResetPlacement");reset.HorizontalAlignment(HorizontalAlignment::Left);reset.Click([this](auto const&,auto const&){Action(L"reset_position");});stack.Children().Append(reset);card.Child(stack);PageHost().Children().Append(card);
    }

    void MainWindow::BuildAbout()
    {
        auto hero = Card();
        auto h = StackPanel(); h.Spacing(10);
        auto mark = Border(); mark.Width(54); mark.Height(54); mark.CornerRadius(CornerRadiusHelper::FromUniformRadius(16));
        mark.Background(Application::Current().Resources().Lookup(box_value(L"AccentFillColorDefaultBrush")).as<Media::Brush>());
        auto glyph = TextBlock(); glyph.Text(L"C"); glyph.FontSize(28); glyph.HorizontalAlignment(HorizontalAlignment::Center); glyph.VerticalAlignment(VerticalAlignment::Center); mark.Child(glyph);
        h.Children().Append(mark); h.Children().Append(Label(L"Codex Usage", 25));
        h.Children().Append(Label(L"A lightweight, portable Windows quota monitor for Codex and Claude Code.", 14));
        h.Children().Append(Label(L"Developed by Zoltán Rózsa", 13));
        versionText_ = Label(state_ ? std::wstring(std::wstring(L"Version ") + state_.GetNamedString(L"version", L"unknown").c_str()) : std::wstring(L"Connecting to the widget…"), 12);
        h.Children().Append(versionText_);
        hero.Child(h); PageHost().Children().Append(hero);

        auto how = Card(); auto howStack = StackPanel(); howStack.Spacing(10);
        howStack.Children().Append(Label(L"How it works", 19));
        howStack.Children().Append(Label(L"Codex Usage runs locally and reuses sign-ins from the Codex CLI and Claude Code CLI already installed on your PC. There is no separate Codex Usage account, login, or API-key setup.", 13));
        howStack.Children().Append(Label(L"It reads quota and reset information from provider services and shows the available usage windows in your taskbar. Quota requests go directly from this app to those services; they do not pass through a Codex Usage server.", 13));
        how.Child(howStack); PageHost().Children().Append(how);

        auto info = Card(); auto s = StackPanel(); s.Spacing(12);
        s.Children().Append(Label(L"Project", 19));
        auto projectRow = StackPanel(); projectRow.Orientation(Orientation::Horizontal); projectRow.Spacing(12); projectRow.VerticalAlignment(VerticalAlignment::Center);
        auto repo = Button(); repo.Content(box_value(L"View project")); Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(repo, L"ViewProject"); repo.HorizontalAlignment(HorizontalAlignment::Left); repo.Click([this](auto const&, auto const&) { Action(L"repository"); });
        auto license = Label(L"Licensed under AGPL-3.0-or-later", 13);
        license.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());
        projectRow.Children().Append(repo); projectRow.Children().Append(license); s.Children().Append(projectRow);

        s.Children().Append(Label(L"Credits & licenses", 17));
        s.Children().Append(Label(L"upstream-ray/codex-usage-monitor — idea and source basis; this fork branched from v1.10 and is now independently developed. MIT.", 13));
        s.Children().Append(Label(L"lobehub/lobe-icons — provider icons. MIT.", 13));
        s.Children().Append(Label(L"CodeZeno/Claude-Code-Usage-Monitor — MIT.", 13));
        s.Children().Append(Label(L"Codex Usage is a maintained derivative of CodeZeno/Claude-Code-Usage-Monitor. Thanks to Craig Constable and the upstream contributors for the original project. Changes in this repository are not affiliated with or endorsed by the upstream maintainers or OpenAI.", 13));
        info.Child(s); PageHost().Children().Append(info);

         auto update=Card();auto u=StackPanel();u.Spacing(10);u.Children().Append(Label(L"Updates",19));updateStatusText_=Label(state_?std::wstring(state_.GetNamedString(L"update_status",L"Update status unavailable").c_str()):std::wstring(L"Checking update status…"),13);u.Children().Append(updateStatusText_);checkForUpdatesButton_=Button();checkForUpdatesButton_.Content(box_value(L"Check for updates"));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(checkForUpdatesButton_,L"CheckForUpdates");checkForUpdatesButton_.Style(Application::Current().Resources().Lookup(box_value(L"AccentButtonStyle")).as<Style>());checkForUpdatesButton_.IsEnabled(!(state_&&state_.GetNamedBoolean(L"update_busy",false)));checkForUpdatesButton_.HorizontalAlignment(HorizontalAlignment::Left);checkForUpdatesButton_.Click([this](auto const&,auto const&){Action(L"update");});u.Children().Append(checkForUpdatesButton_);update.Child(u);PageHost().Children().Append(update);
         auto channel=state_?state_.GetNamedString(L"install_channel",L"portable"):L"portable";auto uninstallCard=Card();auto us=StackPanel();us.Spacing(10);us.Children().Append(Label(L"Uninstall Codex Usage",19));auto warning=StackPanel();warning.Orientation(Orientation::Horizontal);warning.Spacing(8);warning.Children().Append(Label(L"⚠",18));uninstallDescription_=Label(channel==L"winget"?L"This permanently removes the app. WinGet will open its uninstall flow, which asks you to confirm before anything is removed.":L"This permanently removes Codex Usage from this PC. The built-in flow asks you to confirm before anything is removed.",13);warning.Children().Append(uninstallDescription_);us.Children().Append(warning);auto uninstall=Button();uninstall.Content(box_value(L"Uninstall Codex Usage…"));Microsoft::UI::Xaml::Automation::AutomationProperties::SetAutomationId(uninstall,L"UninstallApp");uninstall.Foreground(Media::SolidColorBrush(Windows::UI::Color{255,255,255,255}));uninstall.Background(Media::SolidColorBrush(Windows::UI::Color{255,196,43,28}));uninstall.HorizontalAlignment(HorizontalAlignment::Left);uninstall.Click([this](auto const&,auto const&){Action(L"uninstall");});us.Children().Append(uninstall);uninstallCard.Child(us);PageHost().Children().Append(uninstallCard);
    }
}
