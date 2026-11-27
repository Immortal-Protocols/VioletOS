/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Format.h>
#include <AK/ScopeGuard.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibCore/ArgsParser.h>
#include <LibCore/EventLoop.h>
#include <LibCore/File.h>
#include <LibCore/Process.h>
#include <LibCore/Socket.h>
#include <LibCore/System.h>
#include <LibGfx/Color.h>
#include <LibGfx/Palette.h>
#include <LibIPC/Transport.h>
#include <LibImageDecoderClient/Client.h>
#include <LibMain/Main.h>
#include <LibRequests/RequestControlClient.h>
#include <LibURL/Parser.h>
#include <LibURL/Site.h>
#include <LibWeb/Bindings/MainThreadVM.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Element.h>
#include <LibWeb/Fetch/Fetching/FetchEngine.h>
#include <LibWeb/HTML/HTMLElement.h>
#include <LibWeb/HTML/LocalTraversableNavigable.h>
#include <LibWeb/HTML/Parser/HTMLParser.h>
#include <LibWeb/HTML/Parser/ParserScriptingMode.h>
#include <LibWeb/HTML/Scripting/Agent.h>
#include <LibWeb/Loader/ResourceLoader.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Platform/EventLoopPlugin.h>
#include <LibWeb/Platform/FontPlugin.h>
#include <LibWeb/Platform/RemoteImageCodecPlugin.h>
#include <LibWebCommon/CSS/PreferredColorScheme.h>
#include <LibWebCommon/CSS/PreferredContrast.h>
#include <LibWebCommon/CSS/PreferredMotion.h>
#include <LibWebCommon/HTML/CrossProcessId.h>
#include <LibWebView/CookieJar.h>
#include <LibWebView/ImageDecoderControlClient.h>
#include <unistd.h>

class DocumentPageClient final : public Web::PageClient {
    GC_CELL(DocumentPageClient, Web::PageClient);
    GC_DECLARE_ALLOCATOR(DocumentPageClient);

public:
    static GC::Ref<DocumentPageClient> create(JS::VM& vm)
    {
        return vm.heap().allocate<DocumentPageClient>();
    }

    virtual ~DocumentPageClient() override = default;

    void set_page(GC::Ref<Web::Page> page) { m_page = page; }

    virtual Web::PageId id() const override { return Web::PageId { 0 }; }
    // This page never talks to a UI process, so its ids only need to be unique within it.
    virtual Web::HTML::CrossProcessId allocate_cross_process_id() override { return m_cross_process_id_allocator.allocate(); }
    virtual Web::Page& page() override { return *m_page; }
    virtual Web::Page const& page() const override { return *m_page; }
    virtual bool is_connection_open() const override { return true; }
    virtual Gfx::Palette palette() const override { return Gfx::Palette(*m_palette_impl); }
    virtual Web::DevicePixelRect screen_rect() const override { return {}; }
    virtual double zoom_level() const override { return 1.0; }
    virtual double device_pixel_ratio() const override { return 1.0; }
    virtual double device_pixels_per_css_pixel() const override { return 1.0; }
    virtual Web::CSS::PreferredColorScheme preferred_color_scheme() const override { return Web::CSS::PreferredColorScheme::Auto; }
    virtual Web::CSS::PreferredContrast preferred_contrast() const override { return Web::CSS::PreferredContrast::Auto; }
    virtual Web::CSS::PreferredMotion preferred_motion() const override { return Web::CSS::PreferredMotion::Auto; }
    virtual size_t screen_count() const override { return 1; }
    virtual Queue<Web::QueuedInputEvent>& input_event_queue() override { return m_input_event_queue; }
    virtual void report_finished_handling_input_event([[maybe_unused]] Web::PageId page_id, [[maybe_unused]] u64 event_id, [[maybe_unused]] Web::EventResult event_was_handled) override { }
    bool requires_browser_navigation() const { return m_requires_browser_navigation; }
    virtual void request_navigation_start(Web::HTML::LocalNavigable& navigable, Web::NavigationTarget,
        URL::URL const&, Utf16String navigation_id, Optional<Web::HTML::NavigationStartRequest>) override
    {
        // A new document needs Hub to select its process and validate its origin.
        // Cancel LibWeb's parked navigation and let the frontend handle the page.
        m_requires_browser_navigation = true;
        navigable.resume_navigation_params_creation(navigation_id, {});
    }
    virtual void request_frame() override { }
    virtual void request_file(Web::FileRequest) override { }
    virtual bool is_headless() const override { return true; }

private:
    DocumentPageClient()
    {
        auto buffer = MUST(Core::AnonymousBuffer::create_with_size(sizeof(Gfx::SystemTheme)));
        auto* theme = buffer.data<Gfx::SystemTheme>();
        theme->color[to_underlying(Gfx::ColorRole::Window)] = Color(Color::White).value();
        m_palette_impl = Gfx::PaletteImpl::create_with_anonymous_buffer(buffer);
    }

    virtual void visit_edges(JS::Cell::Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(m_page);
    }

    bool m_requires_browser_navigation { false };
    GC::Ptr<Web::Page> m_page;
    RefPtr<Gfx::PaletteImpl> m_palette_impl;
    Queue<Web::QueuedInputEvent> m_input_event_queue;
    Web::HTML::CrossProcessIdAllocator m_cross_process_id_allocator { .namespace_id = NumericLimits<u64>::max() };
};

GC_DEFINE_ALLOCATOR(DocumentPageClient);

static ErrorOr<NonnullOwnPtr<IPC::Transport>> spawn_service(StringView name, StringView path, ReadonlySpan<ByteString> arguments)
{
    int sockets[2];
    TRY(Core::System::socketpair(AF_LOCAL, SOCK_STREAM | SOCK_CLOEXEC, 0, sockets));
    ArmedScopeGuard close_parent { [&] { (void)Core::System::close(sockets[0]); } };
    ArmedScopeGuard close_child { [&] { (void)Core::System::close(sockets[1]); } };
    Core::ProcessSpawnOptions options {
        .name = name,
        .executable = path,
        .arguments = arguments,
        .environment = Vector<ByteString> { ByteString::formatted("SOCKET_TAKEOVER=ipc:{}", sockets[1]),
            "ICU_DATA=/usr/share/icu/78.3" },
    };
    options.file_actions.append(Core::FileAction::DupFd { .write_fd = sockets[1], .fd = sockets[1] });
    // The renderer returns its DOM on stdout. Diagnostics from the network child belong on stderr.
    options.file_actions.append(Core::FileAction::DupFd { .write_fd = STDERR_FILENO, .fd = STDOUT_FILENO });
    TRY(Core::Process::spawn(options));
    close_child.disarm();
    TRY(Core::System::close(sockets[1]));
    auto socket = TRY(Core::LocalSocket::adopt_fd(sockets[0]));
    close_parent.disarm();
    return make<IPC::Transport>(move(socket));
}

// RequestServer speaks its normal control protocol to the shell, and its request
// protocol directly to LibWeb. The service's own curl and TLS stack perform I/O.
static ErrorOr<NonnullRefPtr<Requests::RequestControlClient>> connect_network(URL::URL const& document_url)
{
    Array<ByteString, 1> arguments { "--http-disk-cache-mode=disabled" };
    auto control = TRY(try_make_ref_counted<Requests::RequestControlClient>(
        TRY(spawn_service("RequestServer"sv, "/opt/ladybird/libexec/RequestServer"sv, arguments))));
    auto response = control->try_connect_new_client(RequestServer::IsPrivate::Yes, RequestServer::SiteBinding::Bound);
    if (response.is_error() || response.value().client_id() < 0)
        return Error::from_string_literal("Failed to connect to RequestServer");
    auto id = response.value().client_id();
    auto site = URL::Site::serialize_for_partitioning(document_url.origin());
    if (site.has_value() && control->try_bind_client_to_site(id, site.value(), site).is_error())
        return Error::from_string_literal("Failed to register the document site with RequestServer");
    auto client = TRY(try_make_ref_counted<Requests::RequestClient>(TRY(response.value().handle().create_transport()), id));
    auto jar = WebView::CookieJar::create(WebView::IsPrivate::Yes);
    auto* cookies = jar.ptr();
    control->on_retrieve_http_cookie = [cookies](int, auto const& url, auto const& partition, auto same_site) {
        return cookies->get_cookie(url, HTTP::Cookie::Source::Http, partition, same_site);
    };
    control->on_store_response_cookies_and_hsts_policy = [jar = move(jar)](int, auto const& url, auto const& partition, auto same_site, auto const& cookies, auto const&) {
        for (auto const& cookie : cookies)
            jar->set_cookie(url, cookie, HTTP::Cookie::Source::Http, partition, same_site);
    };
    Web::ResourceLoader::the().set_client({}, move(client));
    return control;
}

ErrorOr<int> ladybird_main(Main::Arguments arguments)
{
    bool serialize = false;
    StringView input_path;
    StringView address = "about:blank"sv;
    Core::ArgsParser options;
    options.add_option(serialize, "Write the post-script HTML document", "html");
    options.add_option(address, "Original document URL", "url", 0, "URL");
    options.add_positional_argument(input_path, "HTML file, or - for standard input", "file");
    options.parse(arguments);
    auto url = URL::Parser::basic_parse(address);
    if (!url.has_value())
        return Error::from_string_literal("Invalid document URL");
    auto file = input_path == "-"sv ? TRY(Core::File::standard_input()) : TRY(Core::File::open(input_path, Core::File::OpenMode::Read));
    auto bytes = TRY(file->read_until_eof());
    auto& event_loop = Core::EventLoop::initialize_for_current_thread();
    Web::Platform::EventLoopPlugin::install(*new Web::Platform::EventLoopPlugin);
    Web::Platform::FontPlugin::install(*new Web::Platform::FontPlugin(false));
    Web::Bindings::initialize_agent(Web::HTML::AgentType::SimilarOriginWindow);
    auto& agent = Web::HTML::surrounding_agent();
    agent.fetch_executor = Web::Fetch::Fetching::create_inline_fetch_executor(agent.network_activity_observers);
    auto& vm = Web::Bindings::main_thread_vm();
    Web::ResourceLoader::initialize(vm.heap());
    RefPtr<Requests::RequestControlClient> network;
    if (access("/opt/ladybird/libexec/RequestServer", X_OK) == 0)
        network = TRY(connect_network(*url));
    RefPtr<WebView::ImageDecoderControlClient> image_service;
    Web::Platform::ImageCodecPlugin::install(*new Web::Platform::RemoteImageCodecPlugin(
        [&image_service]() -> ErrorOr<NonnullRefPtr<ImageDecoderClient::Client>> {
            if (!image_service || !image_service->is_open()) {
                auto control = TRY(spawn_service("ImageDecoder"sv, "/opt/ladybird/libexec/ImageDecoder"sv, {}));
                image_service = TRY(try_make_ref_counted<WebView::ImageDecoderControlClient>(move(control)));
            }
            auto channel = TRY(IPC::TransportHandle::create_pair());
            image_service->async_connect_new_client(channel.first);
            return try_make_ref_counted<ImageDecoderClient::Client>(TRY(channel.second.create_transport()));
        }, [&image_service] { image_service = nullptr; }));
    ScopeGuard release_image_plugin { [] { Web::Platform::ImageCodecPlugin::shut_down(); } };
    auto client = DocumentPageClient::create(vm);
    auto page = Web::Page::create(client);
    client->set_page(page);
    page->set_is_scripting_enabled(true);
    page->set_top_level_traversable(Web::HTML::LocalTraversableNavigable::create_a_new_top_level_traversable(page, nullptr, {}));
    auto document = page->local_traversable()->active_document();
    document->remove_all_children();
    document->set_origin(url->origin());
    document->set_is_initial_about_blank(false);
    auto parser = Web::HTML::HTMLParser::create_from_byte_string(*document, StringView(bytes), Web::HTML::ParserScriptingMode::Normal, "UTF-8"sv);
    document->update_readiness(Web::HTML::DocumentReadyState::Loading);
    parser->run(*url);
    event_loop.spin_until([&] {
        return client->requires_browser_navigation() || document->readiness() == Web::HTML::DocumentReadyState::Complete;
    });
    if (client->requires_browser_navigation())
        return Error::from_string_literal("Document navigation requires the full Ladybird browser");
    if (serialize) {
        auto html = Web::HTML::HTMLParser::serialize_html_fragment(*document, Web::HTML::HTMLParser::SerializableShadowRoots::No, {});
        outln("{}", html.to_utf8());
        return 0;
    }
    auto body = document->body();
    if (!body)
        return Error::from_string_literal("Page lacks a document body");
    outln("{}", body->inner_text().to_utf8());
    return 0;
}
