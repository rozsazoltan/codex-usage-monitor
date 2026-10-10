#include "pch.h"
#include "MainWindow.xaml.h"
#include "MainWindow.g.cpp"
#include "MainWindow.xaml.g.hpp"
#include <shellapi.h>
#include <algorithm>
#include <array>
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
}

namespace winrt::SettingsUI::implementation
{
    MainWindow::MainWindow()
    {
        InitializeComponent();
        dispatcher_ = Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread();
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd_));
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
        if (inFlight_ != 0) { ShowMessage(L"Please wait for the current setting to finish applying."); return; }
        inFlight_ = nextId_++;
        request.Insert(L"id", JsonValue::CreateNumberValue(inFlight_));
        const auto text = Utf8(request.Stringify().c_str());
        if (text.empty() || text.size() > 65536) { inFlight_ = 0; ShowMessage(L"The request could not be sent.", true); return; }
        { std::lock_guard lock(requestsMutex_); requests_.push(text); }
        requestsChanged_.notify_one(); ShowMessage(L"Applying…");
    }

    void MainWindow::Receive(JsonObject const& message)
    {
        if (message.HasKey(L"event"))
        {
            auto event = message.GetNamedString(L"event", L"");
            if (event == L"shutdown") { stopping_ = true; Close(); return; }
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
                const bool settingsChanged = !state_ || state_.GetNamedObject(L"settings", JsonObject()).Stringify() != updated.GetNamedObject(L"settings", JsonObject()).Stringify();
                state_ = updated;
                // Poll/status events are frequent; avoid rebuilding controls (and
                // stealing keyboard focus) when only non-setting state changed.
                if (settingsChanged || page_ == L"About") Hydrate();
            }
            return;
        }
        if (!message.HasKey(L"id") || message.GetNamedNumber(L"id", 0) != inFlight_) return;
        const bool isInitialGet = initialGetPending_;
        initialGetPending_ = false;
        inFlight_ = 0;
        if (message.HasKey(L"state") && message.GetNamedValue(L"state").ValueType() == JsonValueType::Object) state_ = message.GetNamedObject(L"state");
        Hydrate();
        const bool ok = message.GetNamedBoolean(L"ok", false);
        if (!ok) ShowMessage(std::wstring(message.GetNamedString(L"error", L"The setting could not be applied.").c_str()), true);
        else if (!isInitialGet) ShowMessage(L"Saved. Changes are now in effect.");
    }

    JsonObject MainWindow::State() const { return state_ ? state_.GetNamedObject(L"settings", JsonObject()) : JsonObject(); }
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
        auto request = Request(L"set"); request.Insert(L"key", JsonValue::CreateStringValue(key)); request.Insert(L"value", value); Send(request);
    }
    void MainWindow::Action(std::wstring const& action)
    {
        auto request = Request(L"action"); request.Insert(L"action", JsonValue::CreateStringValue(action)); Send(request);
    }
    void MainWindow::ShowMessage(std::wstring const& text, bool error)
    {
        MessageBar().Message(text); MessageBar().Severity(error ? InfoBarSeverity::Error : InfoBarSeverity::Informational); MessageBar().IsOpen(true);
    }
    void MainWindow::Hydrate() { RenderPage(page_); }

    void MainWindow::OnNavigationChanged(NavigationView const&, NavigationViewSelectionChangedEventArgs const& args)
    {
        auto item = args.SelectedItem().try_as<NavigationViewItem>(); if (!item) return;
        page_ = unbox_value<hstring>(item.Tag()).c_str(); RenderPage(page_);
    }
    void MainWindow::RenderPage(std::wstring const& page)
    {
        page_ = page; PageHost().Children().Clear();
        PageTitle().Text(page); PageEyebrow().Text(page == L"About" ? L"YOUR APP" : L"PREFERENCES");
        const std::map<std::wstring, std::wstring> summaries{
            {L"General",L"A few thoughtful choices make the widget feel like yours."}, {L"Monitoring",L"Choose what to watch and how often to refresh."},
            {L"Appearance",L"Tune the widget’s look without changing its behavior."}, {L"Placement",L"Choose which taskbar and side should host the widget."},
            {L"About",L"A clear view of your usage, right where you need it."}};
        PageSummary().Text(summaries.at(page));
        hydrating_ = true;
        if (page == L"General") BuildGeneral(); else if (page == L"Monitoring") BuildMonitoring(); else if (page == L"Appearance") BuildAppearance(); else if (page == L"Placement") BuildPlacement(); else BuildAbout();
        hydrating_ = false;
    }

    void MainWindow::BuildGeneral()
    {
        auto card = Card(); auto stack = StackPanel(); stack.Spacing(18);
        stack.Children().Append(Label(L"A good place to start", 19));
        auto settings = State();
        auto toggle = [this, &stack](std::wstring key, std::wstring title, std::wstring detail)
        {
            auto row = StackPanel(); row.Orientation(Orientation::Horizontal); row.HorizontalAlignment(HorizontalAlignment::Stretch);
            auto copy = StackPanel(); copy.Spacing(3); copy.HorizontalAlignment(HorizontalAlignment::Stretch); copy.Children().Append(Label(title, 14));
            auto sub = Label(detail, 12); sub.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>()); copy.Children().Append(sub);
            auto sw = ToggleSwitch(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw, title); sw.IsOn(Bool(key)); sw.VerticalAlignment(VerticalAlignment::Center); sw.Toggled([this, key](auto const& sender, auto const&) { SetValue(key, JsonValue::CreateBooleanValue(sender.as<ToggleSwitch>().IsOn())); });
            row.Children().Append(copy); row.Children().Append(sw); stack.Children().Append(row);
        };
        toggle(L"startup", L"Start with Windows", L"Open the widget when you sign in.");
        toggle(L"widget_visible", L"Show the taskbar widget", L"Keep the live usage view visible in your taskbar.");
        auto languageRow = StackPanel(); languageRow.Spacing(8); languageRow.Children().Append(Label(L"Language"));
        auto languages = ComboBox(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(languages, L"Language"); languages.MinWidth(230); languages.HorizontalAlignment(HorizontalAlignment::Left);
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
        auto exit = Button(); exit.Content(box_value(L"Exit Codex Usage")); exit.Click([this](auto const&, auto const&) { Action(L"exit"); }); exitStack.Children().Append(exit);
        exitCard.Child(exitStack); PageHost().Children().Append(exitCard);
    }

    void MainWindow::BuildMonitoring()
    {
        auto providers=Card(); auto p=StackPanel(); p.Spacing(14); p.Children().Append(Label(L"Usage providers",19));
        auto available=state_ && state_.GetNamedBoolean(L"claude_code_available", false);
        auto add=[this,&p,available](std::wstring key,std::wstring name,std::wstring detail,bool enabled,bool unavailable)
        {
            auto row=StackPanel(); row.Orientation(Orientation::Horizontal); auto copy=StackPanel(); copy.Spacing(3); copy.Children().Append(Label(name)); auto d=Label(unavailable?L"Sign in to the Claude CLI to enable monitoring.":detail,12); d.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>()); copy.Children().Append(d);
            auto sw=ToggleSwitch(); Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw,name); sw.IsOn(enabled); sw.IsEnabled(!unavailable); sw.Toggled([this,key](auto const& s,auto const&){SetValue(key,JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));}); row.Children().Append(copy);row.Children().Append(sw);p.Children().Append(row);
        };
        add(L"show_claude_code",L"Claude Code",L"Monitor usage from your Claude CLI account.",Bool(L"show_claude_code"),!available);
        add(L"show_codex",L"Codex",L"Monitor your Codex account.",Bool(L"show_codex"),false);
        add(L"show_antigravity",L"Antigravity",L"Monitor your Antigravity account.",Bool(L"show_antigravity"),false);
        auto note=Label(L"At least one provider must stay enabled.",12);note.Foreground(Application::Current().Resources().Lookup(box_value(L"TextFillColorSecondaryBrush")).as<Media::Brush>());p.Children().Append(note);providers.Child(p);PageHost().Children().Append(providers);
        auto options=Card();auto o=StackPanel();o.Spacing(16);o.Children().Append(Label(L"Refresh and alerts",19));
        auto interval=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(interval,L"Polling interval");interval.MinWidth(220);interval.HorizontalAlignment(HorizontalAlignment::Left);
        for(auto [ms,label]:std::array<std::pair<uint32_t,std::wstring>,4>{{{60000,L"Every minute"},{300000,L"Every 5 minutes"},{900000,L"Every 15 minutes"},{3600000,L"Every hour"}}}){auto it=ComboBoxItem();it.Content(box_value(label));it.Tag(box_value(ms));interval.Items().Append(it);}
        auto current=State().GetNamedNumber(L"poll_interval_ms",900000);for(uint32_t i=0;i<interval.Items().Size();++i)if(unbox_value<uint32_t>(interval.Items().GetAt(i).as<ComboBoxItem>().Tag())==current)interval.SelectedIndex(i);
        interval.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(L"poll_interval_ms",JsonValue::CreateNumberValue(unbox_value<uint32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));});
        auto intervalLabel=StackPanel();intervalLabel.Spacing(7);intervalLabel.Children().Append(Label(L"Polling interval"));intervalLabel.Children().Append(interval);o.Children().Append(intervalLabel);
        auto adaptive=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(adaptive,L"Adaptive refresh");adaptive.Header(box_value(L"Adaptive refresh"));adaptive.OffContent(box_value(L"Off"));adaptive.OnContent(box_value(L"On"));adaptive.IsOn(Bool(L"adaptive_refresh"));adaptive.Toggled([this](auto const&s,auto const&){SetValue(L"adaptive_refresh",JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));});o.Children().Append(adaptive);
        auto wins=StackPanel();wins.Spacing(7);wins.Children().Append(Label(L"Usage windows"));
        for(auto [key,title]:std::array<std::pair<std::wstring,std::wstring>,2>{{{L"show_session_window",L"Session"},{L"show_weekly_window",L"Weekly"}}}){auto sw=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(sw,title);sw.Header(box_value(title));sw.IsOn(Bool(key));sw.Toggled([this,key](auto const&s,auto const&){SetValue(key,JsonValue::CreateBooleanValue(s.as<ToggleSwitch>().IsOn()));});wins.Children().Append(sw);}o.Children().Append(wins);
        auto alerts=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(alerts,L"Low quota alerts");alerts.MinWidth(220);alerts.HorizontalAlignment(HorizontalAlignment::Left);for(auto [n,label]:std::array<std::pair<int,std::wstring>,4>{{{0,L"Off"},{10,L"10% remaining"},{20,L"20% remaining"},{30,L"30% remaining"}}}){auto it=ComboBoxItem();it.Content(box_value(label));it.Tag(box_value(n));alerts.Items().Append(it);}auto threshold=State().GetNamedNumber(L"alert_threshold_percent",0);for(uint32_t i=0;i<alerts.Items().Size();++i)if(unbox_value<int32_t>(alerts.Items().GetAt(i).as<ComboBoxItem>().Tag())==threshold)alerts.SelectedIndex(i);alerts.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(L"alert_threshold_percent",JsonValue::CreateNumberValue(unbox_value<int32_t>(control.SelectedItem().as<ComboBoxItem>().Tag())));});auto ar=StackPanel();ar.Spacing(7);ar.Children().Append(Label(L"Low quota alerts"));ar.Children().Append(alerts);o.Children().Append(ar);
        auto refresh = Button(); refresh.Content(box_value(L"Refresh usage now")); refresh.Click([this](auto const&, auto const&) { Action(L"refresh"); }); o.Children().Append(refresh);
        options.Child(o);PageHost().Children().Append(options);
    }

    void MainWindow::BuildAppearance()
    {
        auto card=Card();auto stack=StackPanel();stack.Spacing(16);stack.Children().Append(Label(L"Widget style",19));
        auto addCombo=[this,&stack](std::wstring title,std::wstring key,std::vector<std::pair<std::wstring,std::wstring>> values){auto group=StackPanel();group.Spacing(6);group.Children().Append(Label(title));auto combo=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(combo,title);combo.MinWidth(250);combo.HorizontalAlignment(HorizontalAlignment::Left);for(auto const& [value,label]:values){auto item=ComboBoxItem();item.Content(box_value(label));item.Tag(box_value(value));combo.Items().Append(item);}auto current=Text(key);for(uint32_t i=0;i<combo.Items().Size();++i)if(unbox_value<hstring>(combo.Items().GetAt(i).as<ComboBoxItem>().Tag())==current)combo.SelectedIndex(i);combo.SelectionChanged([this,key](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem())SetValue(key,JsonValue::CreateStringValue(unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag())));});group.Children().Append(combo);stack.Children().Append(group);};
        addCombo(L"Theme",L"palette",{{L"system",L"Follow Windows"},{L"high_contrast_dark",L"Dark"},{L"high_contrast_light",L"Light"}});addCombo(L"Progress bars",L"bar_style",{{L"continuous",L"Continuous"},{L"segmented",L"Segmented"}});addCombo(L"Bar thickness",L"bar_thickness",{{L"standard",L"Standard"},{L"slim",L"Slim"}});addCombo(L"Text size",L"font_size",{{L"standard",L"Standard"},{L"large",L"Large"}});
        auto buttons=StackPanel();buttons.Orientation(Orientation::Horizontal);buttons.Spacing(10);for(auto [label,action]:std::array<std::pair<std::wstring,std::wstring>,3>{{{L"Recommended",L"recommended"},{L"Reset appearance",L"reset_appearance"},{L"Reset colors",L"reset_colors"}}}){auto b=Button();b.Content(box_value(label));b.Click([this,action](auto const&,auto const&){Action(action);});buttons.Children().Append(b);}stack.Children().Append(buttons);
        card.Child(stack);PageHost().Children().Append(card);
        auto colors=Card();auto c=StackPanel();c.Spacing(12);c.Children().Append(Label(L"Provider colors",19));c.Children().Append(Label(L"Use the widget’s recommended colors, or set a custom RGB accent from hex." ,12));
        for(auto const& [key,title]:std::array<std::pair<std::wstring,std::wstring>,2>{{{L"claude_accent",L"Claude Code accent"},{L"codex_accent",L"Codex accent"}}}){auto row=StackPanel();row.Orientation(Orientation::Horizontal);row.Spacing(8);auto input=TextBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(input,title);input.PlaceholderText(L"Default color");input.Width(145);auto val=State().GetNamedValue(key,JsonValue::CreateNullValue());if(val.ValueType()==JsonValueType::Array){auto a=val.GetArray();if(a.Size()==3){std::wostringstream hex;hex<<L"#"<<std::uppercase<<std::hex<<std::setfill(L'0');for(auto const& n:a)hex<<std::setw(2)<<static_cast<int>(n.GetNumber());input.Text(hex.str());}}auto set=Button();set.Content(box_value(L"Apply color"));set.Click([this,input,key] (auto const&,auto const&){auto text=input.Text().c_str();unsigned r,g,b;if(swscanf_s(text,L"#%2x%2x%2x",&r,&g,&b)!=3){ShowMessage(L"Enter a color like #D97757.",true);return;}auto arr=JsonArray();arr.Append(JsonValue::CreateNumberValue(r));arr.Append(JsonValue::CreateNumberValue(g));arr.Append(JsonValue::CreateNumberValue(b));SetValue(key,arr);});row.Children().Append(Label(title));row.Children().Append(input);row.Children().Append(set);c.Children().Append(row);}
        colors.Child(c);PageHost().Children().Append(colors);
    }

    void MainWindow::BuildPlacement()
    {
        auto card=Card();auto stack=StackPanel();stack.Spacing(16);stack.Children().Append(Label(L"Taskbar placement",19));stack.Children().Append(Label(L"Choose the display where the widget should appear. The widget stays aligned with the taskbar.",13));auto monitors=ComboBox();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(monitors,L"Monitor device");monitors.MinWidth(360);monitors.HorizontalAlignment(HorizontalAlignment::Left);auto automatic=ComboBoxItem();automatic.Content(box_value(L"Automatic (primary display)"));automatic.Tag(box_value(L""));monitors.Items().Append(automatic);if(state_&&state_.HasKey(L"monitors"))for(auto const& v:state_.GetNamedArray(L"monitors")){auto m=v.GetObject();auto device=m.GetNamedString(L"device",L"");auto item=ComboBoxItem();item.Content(box_value(std::wstring(m.GetNamedBoolean(L"primary",false)?L"Primary display · ":L"Display · ")+device));item.Tag(box_value(device));monitors.Items().Append(item);}monitors.SelectedIndex(0);auto current=Text(L"monitor_device");if(state_&&State().HasKey(L"monitor_device")&&State().GetNamedValue(L"monitor_device").ValueType()==JsonValueType::String)current=std::wstring(State().GetNamedString(L"monitor_device").c_str());for(uint32_t i=0;i<monitors.Items().Size();++i)if(std::wstring(unbox_value<hstring>(monitors.Items().GetAt(i).as<ComboBoxItem>().Tag()).c_str())==current)monitors.SelectedIndex(i);monitors.SelectionChanged([this](auto const&sender,auto const&){auto control=sender.as<ComboBox>();if(!hydrating_&&control.SelectedItem()){auto val=unbox_value<hstring>(control.SelectedItem().as<ComboBoxItem>().Tag());SetValue(L"monitor_device",val.empty()?JsonValue::CreateNullValue():JsonValue::CreateStringValue(val));}});stack.Children().Append(monitors);
        auto side=ToggleSwitch();Microsoft::UI::Xaml::Automation::AutomationProperties::SetName(side,L"Anchor to the left");side.Header(box_value(L"Anchor to the left"));side.OffContent(box_value(L"Right"));side.OnContent(box_value(L"Left"));side.IsOn(Bool(L"anchor_left"));side.Toggled([this](auto const&sender,auto const&){auto control=sender.as<ToggleSwitch>();SetValue(L"anchor_left",JsonValue::CreateBooleanValue(control.IsOn()));});stack.Children().Append(side);auto reset=Button();reset.Content(box_value(L"Reset position"));reset.Click([this](auto const&,auto const&){Action(L"reset_position");});stack.Children().Append(reset);card.Child(stack);PageHost().Children().Append(card);
    }

    void MainWindow::BuildAbout()
    {
        auto hero=Card();auto h=StackPanel();h.Spacing(10);auto mark=Border();mark.Width(54);mark.Height(54);mark.CornerRadius(CornerRadiusHelper::FromUniformRadius(16));mark.Background(Application::Current().Resources().Lookup(box_value(L"AccentFillColorDefaultBrush")).as<Media::Brush>());auto glyph=TextBlock();glyph.Text(L"C");glyph.FontSize(28);glyph.HorizontalAlignment(HorizontalAlignment::Center);glyph.VerticalAlignment(VerticalAlignment::Center);mark.Child(glyph);h.Children().Append(mark);h.Children().Append(Label(L"Codex Usage",25));h.Children().Append(Label(L"A calm, native taskbar view of your AI coding quota.",14));h.Children().Append(Label(state_?std::wstring((std::wstring(L"Version ")+state_.GetNamedString(L"version",L"unknown").c_str()).c_str()):std::wstring(L"Connecting to the widget…"),12));hero.Child(h);PageHost().Children().Append(hero);
        auto info=Card();auto s=StackPanel();s.Spacing(12);s.Children().Append(Label(L"Project",19));s.Children().Append(Label(L"Open source, built for Windows. Licensed under AGPL-3.0-or-later.",13));auto repo=Button();repo.Content(box_value(L"View project on GitHub"));repo.Click([this](auto const&,auto const&){Action(L"repository");});s.Children().Append(repo);info.Child(s);PageHost().Children().Append(info);
        auto update=Card();auto u=StackPanel();u.Spacing(10);u.Children().Append(Label(L"Updates",19));u.Children().Append(Label(state_?std::wstring(state_.GetNamedString(L"update_status",L"Update status unavailable").c_str()):std::wstring(L"Checking update status…"),13));auto check=Button();check.Content(box_value(L"Check for updates"));check.IsEnabled(!(state_&&state_.GetNamedBoolean(L"update_busy",false)));check.Click([this](auto const&,auto const&){Action(L"update");});u.Children().Append(check);update.Child(u);PageHost().Children().Append(update);
        auto channel=state_?state_.GetNamedString(L"install_channel",L"portable"):L"portable";auto uninstallCard=Card();auto us=StackPanel();us.Spacing(10);us.Children().Append(Label(L"Uninstall",19));us.Children().Append(Label(channel==L"winget"?L"This app was installed with WinGet. Uninstalling opens the app’s safe, confirmed removal flow.":L"Remove Codex Usage using its built-in confirmation flow.",13));auto uninstall=Button();uninstall.Content(box_value(L"Uninstall Codex Usage"));uninstall.Click([this](auto const&,auto const&){Action(L"uninstall");});us.Children().Append(uninstall);uninstallCard.Child(us);PageHost().Children().Append(uninstallCard);
    }
}
