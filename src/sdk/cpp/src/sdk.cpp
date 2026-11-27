// MVF C++ module SDK implementation. Mirrors src/sdk/python/mvf_sdk/__init__.py:
// the same control loop (protocol v1 JSON lines, or v2 binary frames when the engine
// offers them), the same 192-byte typed descriptor, the same shared-memory arena
// addressed by MVF_ARENA_PATH. Little-endian hosts only (x86-64 / arm64), matching
// the on-wire descriptor and frame layouts.

#include "mvf/sdk.hpp"

#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string_view>

#if defined(_WIN32)
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  include <fcntl.h>
#  include <io.h>
#  include <windows.h>
#else
#  include <fcntl.h>
#  include <sys/mman.h>
#  include <sys/stat.h>
#  include <unistd.h>
#endif

namespace mvf {

size_t element_size(ElementType type) {
    switch (type) {
        case ElementType::UInt8:
        case ElementType::Int8:    return 1;
        case ElementType::UInt16:
        case ElementType::Int16:
        case ElementType::Float16:
        case ElementType::BFloat16: return 2;
        case ElementType::UInt32:
        case ElementType::Int32:
        case ElementType::Float32:  return 4;
        case ElementType::UInt64:
        case ElementType::Int64:
        case ElementType::Float64:  return 8;
    }
    throw std::runtime_error("unknown element type");
}

Output blob(std::string bytes) {
    Output o;
    o.shape = { static_cast<int64_t>(bytes.size()) };
    o.media_type = MediaType::Blob;
    o.element_type = ElementType::UInt8;
    o.data = std::move(bytes);
    return o;
}

Output blob(const void* data, size_t size) {
    return blob(std::string(static_cast<const char*>(data), size));
}

Output tensor(std::string bytes, ElementType element_type, std::vector<int64_t> shape,
              MediaType media_type) {
    Output o;
    o.data = std::move(bytes);
    o.media_type = media_type;
    o.element_type = element_type;
    o.shape = std::move(shape);
    return o;
}

namespace {

// ---- little-endian scalar access into the mapped arena ----
template <typename T>
T read_scalar(const uint8_t* p) { T v; std::memcpy(&v, p, sizeof(T)); return v; }
template <typename T>
void write_scalar(uint8_t* p, T v) { std::memcpy(p, &v, sizeof(T)); }

// ---- cross-platform shared-memory arena ----
class Arena {
public:
    ~Arena() {
#if defined(_WIN32)
        if (base_) UnmapViewOfFile(base_);
        if (mapping_) CloseHandle(mapping_);
        if (file_ != INVALID_HANDLE_VALUE) CloseHandle(file_);
#else
        if (base_ && base_ != MAP_FAILED) munmap(base_, size_);
        if (fd_ >= 0) ::close(fd_);
#endif
    }

    bool open(const char* path, bool writable) {
#if defined(_WIN32)
        DWORD access = writable ? (GENERIC_READ | GENERIC_WRITE) : GENERIC_READ;
        file_ = CreateFileA(path, access, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                            OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (file_ == INVALID_HANDLE_VALUE) return false;
        LARGE_INTEGER sz;
        if (!GetFileSizeEx(file_, &sz)) return false;
        size_ = static_cast<size_t>(sz.QuadPart);
        mapping_ = CreateFileMappingA(file_, nullptr,
                                      writable ? PAGE_READWRITE : PAGE_READONLY, 0, 0, nullptr);
        if (!mapping_) return false;
        void* p = MapViewOfFile(mapping_, writable ? FILE_MAP_WRITE : FILE_MAP_READ, 0, 0, 0);
        if (!p) return false;
        base_ = static_cast<uint8_t*>(p);
        return true;
#else
        fd_ = ::open(path, writable ? O_RDWR : O_RDONLY);
        if (fd_ < 0) return false;
        struct stat st{};
        if (fstat(fd_, &st) != 0) return false;
        size_ = static_cast<size_t>(st.st_size);
        int prot = writable ? (PROT_READ | PROT_WRITE) : PROT_READ;
        void* p = mmap(nullptr, size_, prot, MAP_SHARED, fd_, 0);
        if (p == MAP_FAILED) return false;
        base_ = static_cast<uint8_t*>(p);
        return true;
#endif
    }

    uint8_t* base() const { return base_; }

private:
    uint8_t* base_ = nullptr;
    size_t size_ = 0;
#if defined(_WIN32)
    HANDLE file_ = INVALID_HANDLE_VALUE;
    HANDLE mapping_ = nullptr;
#else
    int fd_ = -1;
#endif
};

// ---- descriptor read/write (must match Mvf.Abstractions.PayloadDescriptor) ----

Payload read_descriptor(uint8_t* base, int64_t offset) {
    const uint8_t* h = base + offset;
    if (read_scalar<uint32_t>(h + 0) != kMagic || read_scalar<uint16_t>(h + 4) != kVersion)
        throw std::runtime_error("bad payload header (magic/version)");
    Payload p;
    p.media_type = static_cast<MediaType>(read_scalar<uint16_t>(h + 12));
    p.element_type = static_cast<ElementType>(h[14]);
    uint8_t rank = h[15];
    if (rank > kMaxRank) throw std::runtime_error("payload rank exceeds max");
    int64_t length = read_scalar<int64_t>(h + 16);
    p.shape.resize(rank);
    for (int i = 0; i < rank; ++i) p.shape[i] = read_scalar<int64_t>(h + 24 + i * 8);
    p.data = base + offset + kHeaderSize;
    p.size = static_cast<size_t>(length);
    return p;
}

void write_descriptor(uint8_t* base, int64_t offset, const Output& out, int64_t capacity) {
    const int rank = static_cast<int>(out.shape.size());
    if (rank > kMaxRank) throw std::runtime_error("output rank exceeds max");
    const int64_t esz = static_cast<int64_t>(element_size(out.element_type));
    int64_t length = esz;
    for (int64_t dim : out.shape) length *= dim;
    if (length != static_cast<int64_t>(out.data.size()))
        throw std::runtime_error("output payload size disagrees with its descriptor");
    if (length > capacity)
        throw std::runtime_error("output payload exceeds the reserved slot capacity");

    std::vector<int64_t> strides(rank);
    int64_t running = esz;
    for (int i = rank - 1; i >= 0; --i) { strides[i] = running; running *= out.shape[i]; }

    uint8_t* h = base + offset;
    write_scalar<uint32_t>(h + 0, kMagic);
    write_scalar<uint16_t>(h + 4, kVersion);
    h[6] = 1;                              // flags: C-contiguous
    write_scalar<uint32_t>(h + 8, 0);      // epoch
    write_scalar<uint16_t>(h + 12, static_cast<uint16_t>(out.media_type));
    h[14] = static_cast<uint8_t>(out.element_type);
    h[15] = static_cast<uint8_t>(rank);
    write_scalar<int64_t>(h + 16, length);
    for (int i = 0; i < rank; ++i) {
        write_scalar<int64_t>(h + 24 + i * 8, out.shape[i]);
        write_scalar<int64_t>(h + 88 + i * 8, strides[i]);
    }
    std::memcpy(h + kHeaderSize, out.data.data(), static_cast<size_t>(length));
}

// ---- control channel: v1 JSON lines or v2 binary frames (#32, protocol/worker-v2.md) ----

// A request from the engine, whichever protocol carried it.
struct Request {
    enum class Type { Execute, Checkpoint, Restore, Shutdown, Invalid };
    Type type = Type::Invalid;
    json id;              // v1: the request's id as sent, echoed back (null when absent)
    uint64_t rid = 0;     // v2: the correlation id
    int64_t input = -1;   // execute: the input slot; restore: the state slot (-1 = none)
    int64_t out = -1;     // execute/checkpoint: the output slot (-1 = none)
    int64_t capacity = 0;
    const json* meta = nullptr; // execute: the frame metadata handed to the module
    std::string problem;  // Invalid: why
};

// A result, whichever protocol carries it.
struct Reply {
    enum class Frame { Absent, Dropped, Written };
    Frame frame = Frame::Absent;
    int64_t offset = 0;
    json classification; // null = none
    json value;          // null = none

    void reset() {
        frame = Frame::Absent;
        offset = 0;
        classification = nullptr;
        value = nullptr;
    }
};

// The engine broke the protocol (bad frame): the module stops, it never resynchronises.
struct ProtocolError : std::runtime_error {
    using std::runtime_error::runtime_error;
};

class Wire {
public:
    virtual ~Wire() = default;
    virtual void hello(const std::string& module_id, const char* capability, bool ready) = 0;
    virtual void ready(const std::string& module_id) = 0;
    // The next request; false at the end of the stream.
    virtual bool next(Request& req) = 0;
    virtual void result(const Request& req, const Reply& reply) = 0;
    virtual void error(const Request& req, const std::string& message) = 0;
    virtual void state(const Request& req, bool empty) = 0;
    virtual void restored(const Request& req) = 0;
    virtual void log(const std::string& level, const std::string& message) = 0;
};

// Protocol v1: one JSON object per line on stdin/stdout.
class V1 final : public Wire {
public:
    void hello(const std::string& module_id, const char* capability, bool ready) override {
        json h = {{"type", "hello"}, {"protocol", 1}, {"moduleId", module_id},
                  {"capability", capability}};
        if (!ready) h["ready"] = false;
        send(h);
    }

    void ready(const std::string& module_id) override {
        send({{"type", "ready"}, {"moduleId", module_id}});
    }

    bool next(Request& req) override {
        std::string line;
        while (std::getline(std::cin, line)) {
            if (line.empty()) continue;
            json msg = json::parse(line, nullptr, /*allow_exceptions=*/false);
            if (msg.is_discarded()) continue;
            const std::string type = msg.value("type", "");
            req = Request{};
            req.id = msg.contains("id") ? msg["id"] : json(nullptr);
            try {
                if (type == "shutdown") {
                    req.type = Request::Type::Shutdown;
                } else if (type == "execute") {
                    req.type = Request::Type::Execute;
                    meta_ = msg.contains("frame") ? msg["frame"] : json::object();
                    req.meta = &meta_;
                    req.input = shm_offset(meta_);
                    slot(msg, req);
                } else if (type == "checkpoint") {
                    req.type = Request::Type::Checkpoint;
                    slot(msg, req);
                } else if (type == "restore") {
                    req.type = Request::Type::Restore;
                    req.input = shm_offset(msg);
                } else {
                    continue; // a message this module does not handle
                }
            } catch (const std::exception& exc) {
                req.type = Request::Type::Invalid;
                req.problem = exc.what();
            }
            return true;
        }
        return false;
    }

    void result(const Request& req, const Reply& reply) override {
        json r = {{"type", "result"}, {"id", req.id}};
        if (reply.frame == Reply::Frame::Dropped) r["frame"] = nullptr;
        if (reply.frame == Reply::Frame::Written)
            r["frame"] = {{"shm", {{"offset", reply.offset}}}};
        if (!reply.classification.is_null()) r["classification"] = reply.classification;
        if (!reply.value.is_null()) r["value"] = reply.value;
        send(r);
    }

    void error(const Request& req, const std::string& message) override {
        send({{"type", "error"}, {"id", req.id}, {"message", message}});
    }

    void state(const Request& req, bool empty) override {
        json s = {{"type", "state"}, {"id", req.id}};
        if (empty) s["empty"] = true;
        send(s);
    }

    void restored(const Request& req) override {
        send({{"type", "restored"}, {"id", req.id}});
    }

    void log(const std::string& level, const std::string& message) override {
        send({{"type", "log"}, {"level", level}, {"message", message}});
    }

private:
    static void send(const json& obj) {
        std::cout << obj.dump() << "\n";
        std::cout.flush();
    }

    static int64_t shm_offset(const json& holder) {
        if (!holder.contains("shm") || !holder["shm"].contains("offset")) return -1;
        return holder["shm"]["offset"].get<int64_t>();
    }

    static void slot(const json& msg, Request& req) {
        if (!msg.contains("out")) return;
        const json& out = msg["out"];
        req.out = out.at("offset").get<int64_t>();
        req.capacity = out.at("capacity").get<int64_t>();
    }

    json meta_ = json::object();
};

// Protocol v2: length-prefixed binary frames on the inherited stdio pipes.
//
// At start the channel is moved off the standard descriptors: the SDK keeps private
// duplicates of fd 0 and fd 1 for the protocol, then points fd 1 at stderr and fd 0 at
// the null device. A stray printf or a native library writing to stdout then lands in
// the logs instead of corrupting the protocol. Logs go to stderr as JSON lines.
class V2 final : public Wire {
public:
    V2() {
        const char* session = std::getenv("MVF_SESSION");
        session_ = session ? session : "";
        const char* generation = std::getenv("MVF_GENERATION");
        generation_ = generation && *generation
            ? static_cast<uint32_t>(std::strtoul(generation, nullptr, 10)) : 1u;
        protect();
    }

    void hello(const std::string& module_id, const char* capability, bool ready) override {
        json h = {{"type", "hello"}, {"protocol", 2}, {"moduleId", module_id},
                  {"capability", capability}, {"session", session_}};
        if (!ready) h["ready"] = false;
        send_json(kHello, 0, h);
    }

    void ready(const std::string& module_id) override {
        send_json(kReady, 0, {{"type", "ready"}, {"moduleId", module_id}});
    }

    bool next(Request& req) override {
        for (;;) {
            if (!need(kHeader)) return false;
            const uint8_t* h = buf_.data() + start_;
            if (h[0] != 'M' || h[1] != 'V' || h[2] != 2)
                throw ProtocolError("protocol v2: bad frame from the engine");
            const uint8_t kind = h[3];
            const uint64_t id = read_scalar<uint64_t>(h + 8);
            const uint32_t generation = read_scalar<uint32_t>(h + 16);
            const uint32_t length = read_scalar<uint32_t>(h + 20);
            if (length > kMaxBody)
                throw ProtocolError("protocol v2: a frame from the engine exceeds 1 MiB");
            if (!need(kHeader + length)) return false;
            const uint8_t* b = buf_.data() + start_ + kHeader;
            start_ += kHeader + length;
            generation_ = generation;

            req.rid = id;
            req.input = -1;
            req.out = -1;
            req.capacity = 0;
            req.meta = &meta_;
            switch (kind) {
                case kExecute:
                    req.type = Request::Type::Execute;
                    execute(b, length, req);
                    return true;
                case kCheckpoint:
                    if (length != 16) throw ProtocolError("protocol v2: malformed checkpoint");
                    req.type = Request::Type::Checkpoint;
                    req.out = static_cast<int64_t>(read_scalar<uint64_t>(b));
                    req.capacity = static_cast<int64_t>(read_scalar<uint64_t>(b + 8));
                    return true;
                case kRestore:
                    if (length < 8) throw ProtocolError("protocol v2: malformed restore");
                    req.type = Request::Type::Restore;
                    req.input = static_cast<int64_t>(read_scalar<uint64_t>(b));
                    return true;
                case kPing:
                    frame(kPong, id, nullptr, 0);
                    continue;
                case kShutdown:
                    req.type = Request::Type::Shutdown;
                    return true;
                default:
                    // cancel: this loop is single-threaded, so a cancel arrives after its request
                    // finished; the engine's deadline bounds a slow request. configure: never sent,
                    // this SDK does not advertise the feature.
                    continue;
            }
        }
    }

    void result(const Request& req, const Reply& reply) override {
        uint32_t flags = 0;
        if (reply.frame == Reply::Frame::Dropped) flags |= 2;
        if (reply.frame == Reply::Frame::Written) flags |= 1;
        std::string tail;
        if (!reply.classification.is_null() || !reply.value.is_null()) {
            json t = json::object();
            if (!reply.classification.is_null()) t["classification"] = reply.classification;
            if (!reply.value.is_null()) t["value"] = reply.value;
            tail = t.dump();
            flags |= 8;
        }
        body_.resize(24 + tail.size());
        write_scalar<uint32_t>(body_.data(), flags);
        write_scalar<uint32_t>(body_.data() + 4, 0);
        write_scalar<uint64_t>(body_.data() + 8,
            reply.frame == Reply::Frame::Written ? static_cast<uint64_t>(reply.offset) : 0);
        write_scalar<uint64_t>(body_.data() + 16, 0);
        if (!tail.empty()) std::memcpy(body_.data() + 24, tail.data(), tail.size());
        frame(kResult, req.rid, body_.data(), body_.size());
    }

    void error(const Request& req, const std::string& message) override {
        send_json(kError, req.rid, {{"message", message}});
    }

    void state(const Request& req, bool empty) override {
        frame(kState, req.rid, nullptr, 0, empty ? 1 : 0);
    }

    void restored(const Request& req) override {
        frame(kRestored, req.rid, nullptr, 0);
    }

    void log(const std::string& level, const std::string& message) override {
        const std::string line = json{{"level", level}, {"message", message}}.dump() + "\n";
        write_all(2, line.data(), line.size());
    }

private:
    static constexpr size_t kHeader = 24;
    static constexpr uint32_t kMaxBody = 1u << 20;
    static constexpr uint64_t kNone = ~uint64_t{0};
    enum : uint8_t {
        kHello = 1, kReady = 2, kExecute = 3, kResult = 4, kError = 5, kCheckpoint = 8,
        kState = 9, kRestore = 10, kRestored = 11, kPing = 12, kPong = 13, kShutdown = 15,
    };

    // Moves the protocol onto private descriptors and fd 1 onto stderr.
    void protect() {
#if defined(_WIN32)
        in_ = _dup(0);
        out_ = _dup(1);
        if (in_ < 0 || out_ < 0) throw std::runtime_error("protocol v2: cannot duplicate stdio");
        _setmode(in_, _O_BINARY);
        _setmode(out_, _O_BINARY);
        // A module's own child processes must not inherit the protocol handles.
        SetHandleInformation(reinterpret_cast<HANDLE>(_get_osfhandle(in_)), HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(reinterpret_cast<HANDLE>(_get_osfhandle(out_)), HANDLE_FLAG_INHERIT, 0);
        const int null = _open("NUL", _O_RDONLY);
        if (null >= 0) {
            _dup2(null, 0);
            _close(null);
        }
        _dup2(2, 1);
        // Code that writes through the Win32 handles rather than the CRT sees stderr too.
        SetStdHandle(STD_INPUT_HANDLE, reinterpret_cast<HANDLE>(_get_osfhandle(0)));
        SetStdHandle(STD_OUTPUT_HANDLE, reinterpret_cast<HANDLE>(_get_osfhandle(1)));
        std::setvbuf(stdout, nullptr, _IONBF, 0);
#else
        in_ = ::dup(0);
        out_ = ::dup(1);
        if (in_ < 0 || out_ < 0) throw std::runtime_error("protocol v2: cannot duplicate stdio");
        ::fcntl(in_, F_SETFD, FD_CLOEXEC);
        ::fcntl(out_, F_SETFD, FD_CLOEXEC);
        const int null = ::open("/dev/null", O_RDONLY);
        if (null >= 0) {
            ::dup2(null, 0);
            ::close(null);
        }
        ::dup2(2, 1);
        // fd 1 now feeds the logs: flush it per line, not when a pipe buffer fills.
        std::setvbuf(stdout, nullptr, _IOLBF, BUFSIZ);
#endif
    }

    static long raw_read(int fd, void* p, size_t n) {
#if defined(_WIN32)
        return _read(fd, p, static_cast<unsigned>(n));
#else
        return static_cast<long>(::read(fd, p, n));
#endif
    }

    static void write_all(int fd, const void* data, size_t n) {
        const char* p = static_cast<const char*>(data);
        while (n > 0) {
#if defined(_WIN32)
            const long w = _write(fd, p, static_cast<unsigned>(n));
#else
            const long w = static_cast<long>(::write(fd, p, n));
#endif
            if (w < 0) {
                if (errno == EINTR) continue;
                throw std::runtime_error("protocol v2: write failed");
            }
            p += w;
            n -= static_cast<size_t>(w);
        }
    }

    // Ensures `n` unread bytes are buffered; false at the end of the stream. Usually one
    // read returns a whole request, so a request costs one system call.
    bool need(size_t n) {
        if (end_ - start_ >= n) return true;
        if (start_ > 0) {
            std::memmove(buf_.data(), buf_.data() + start_, end_ - start_);
            end_ -= start_;
            start_ = 0;
        }
        if (buf_.size() < n) buf_.resize(std::max(n, buf_.size() * 2));
        while (end_ < n) {
            const long r = raw_read(in_, buf_.data() + end_, buf_.size() - end_);
            if (r < 0 && errno == EINTR) continue;
            if (r <= 0) return false;
            end_ += static_cast<size_t>(r);
        }
        return true;
    }

    void execute(const uint8_t* b, uint32_t length, Request& req) {
        if (length < 40) throw ProtocolError("protocol v2: malformed execute");
        const uint64_t input = read_scalar<uint64_t>(b);
        const uint64_t sequence = read_scalar<uint64_t>(b + 8);
        const uint64_t out = read_scalar<uint64_t>(b + 16);
        const uint64_t capacity = read_scalar<uint64_t>(b + 24);
        const uint16_t camera_len = read_scalar<uint16_t>(b + 36);
        const uint16_t content_len = read_scalar<uint16_t>(b + 38);
        if (40u + camera_len + content_len > length)
            throw ProtocolError("protocol v2: malformed execute");
        req.out = out == kNone ? -1 : static_cast<int64_t>(out);
        req.capacity = static_cast<int64_t>(capacity);
        if (input == kNone) {
            meta_ = json::object();
            camera_.clear();
            content_.clear();
            return;
        }
        req.input = static_cast<int64_t>(input);
        // The metadata object is reused across frames; only what changed is rewritten.
        const std::string_view camera(reinterpret_cast<const char*>(b + 40), camera_len);
        const std::string_view content(reinterpret_cast<const char*>(b + 40 + camera_len),
                                       content_len);
        if (!meta_.contains("cameraId") || camera != camera_) {
            camera_.assign(camera);
            meta_["cameraId"] = camera_;
        }
        if (!meta_.contains("contentType") || content != content_) {
            content_.assign(content);
            meta_["contentType"] = content_;
        }
        meta_["sequence"] = sequence;
        meta_["shm"]["offset"] = input;
    }

    void frame(uint8_t kind, uint64_t id, const void* body, size_t length, uint16_t flags = 0) {
        tx_.resize(kHeader + length);
        uint8_t* h = tx_.data();
        h[0] = 'M';
        h[1] = 'V';
        h[2] = 2;
        h[3] = kind;
        write_scalar<uint16_t>(h + 4, flags);
        write_scalar<uint16_t>(h + 6, 0);
        write_scalar<uint64_t>(h + 8, id);
        write_scalar<uint32_t>(h + 16, generation_);
        write_scalar<uint32_t>(h + 20, static_cast<uint32_t>(length));
        if (length) std::memcpy(h + kHeader, body, length);
        write_all(out_, tx_.data(), tx_.size()); // one write per frame
    }

    void send_json(uint8_t kind, uint64_t id, const json& body) {
        const std::string text = body.dump();
        frame(kind, id, text.data(), text.size());
    }

    int in_ = -1;
    int out_ = -1;
    uint32_t generation_ = 1;
    std::string session_;
    std::vector<uint8_t> buf_ = std::vector<uint8_t>(65536);
    size_t start_ = 0;
    size_t end_ = 0;
    std::vector<uint8_t> tx_;
    std::vector<uint8_t> body_;
    json meta_ = json::object();
    std::string camera_;
    std::string content_;
};

// The control channel, chosen on first use: v2 when the engine offers it (MVF_PROTOCOL=2 with its
// launch token, MVF_SESSION), otherwise v1. A stray MVF_PROTOCOL=2 in the environment of a host that
// never offers v2 therefore does not switch the module. A log() before the loop starts already uses
// the right channel.
Wire& wire() {
    static std::unique_ptr<Wire> w = [] () -> std::unique_ptr<Wire> {
        const char* offer = std::getenv("MVF_PROTOCOL");
        const char* session = std::getenv("MVF_SESSION");
        if (offer && std::string_view(offer) == "2" && session && *session)
            return std::make_unique<V2>();
        return std::make_unique<V1>();
    }();
    return *w;
}

Payload read_input(uint8_t* base, int64_t offset) {
    if (offset < 0) throw std::runtime_error("frame is missing its shared-memory handle");
    return read_descriptor(base, offset);
}

json optional_json(const std::optional<double>& v) { return v ? json(*v) : json(nullptr); }
json optional_json(const std::optional<std::string>& v) { return v ? json(*v) : json(nullptr); }

json classification_json(const Classification& c) {
    return {
        {"label", c.label},
        {"measurement", optional_json(c.measurement)},
        {"unit", optional_json(c.unit)},
        {"details", optional_json(c.details)},
    };
}

void write_frame(uint8_t* base, const Request& req, const Output& out, Reply& reply) {
    if (req.out < 0) throw std::runtime_error("execute is missing its output slot");
    write_descriptor(base, req.out, out, req.capacity);
    reply.frame = Reply::Frame::Written;
    reply.offset = req.out;
}

using ExecuteFn = std::function<void(const Request& req, uint8_t* base, Reply& reply)>;

int serve(const std::string& module_id, const char* capability, bool writable,
          const ExecuteFn& on_execute, const ModuleHooks& hooks) {
    const char* path = std::getenv("MVF_ARENA_PATH");
    if (!path) {
        std::cerr << "MVF_ARENA_PATH is not set — the shared-memory data plane is required.\n";
        return 1;
    }
    Arena arena;
    if (!arena.open(path, writable)) {
        std::cerr << "failed to map arena at " << path << "\n";
        return 1;
    }
    uint8_t* base = arena.base();
    Wire& w = wire();

    if (hooks.on_start) {
        w.hello(module_id, capability, /*ready=*/false);
        hooks.on_start();
        w.ready(module_id);
    } else {
        w.hello(module_id, capability, /*ready=*/true);
    }

    Request req;
    Reply reply;
    try {
        while (w.next(req)) {
            if (req.type == Request::Type::Shutdown) break;
            try {
                switch (req.type) {
                    case Request::Type::Execute:
                        reply.reset();
                        on_execute(req, base, reply);
                        w.result(req, reply);
                        break;
                    case Request::Type::Checkpoint: {
                        std::optional<Output> state = hooks.on_checkpoint ? hooks.on_checkpoint()
                                                                          : std::nullopt;
                        if (!state) {
                            w.state(req, /*empty=*/true);
                        } else {
                            if (req.out < 0)
                                throw std::runtime_error("checkpoint is missing its state slot");
                            write_descriptor(base, req.out, *state, req.capacity);
                            w.state(req, /*empty=*/false);
                        }
                        break;
                    }
                    case Request::Type::Restore: {
                        Payload p = read_input(base, req.input);
                        if (hooks.on_restore) hooks.on_restore(p);
                        w.restored(req);
                        break;
                    }
                    case Request::Type::Invalid:
                        throw std::runtime_error(req.problem);
                    case Request::Type::Shutdown:
                        break;
                }
            } catch (const ProtocolError&) {
                throw;
            } catch (const std::exception& exc) {
                w.error(req, exc.what());
            }
        }
    } catch (const ProtocolError& exc) {
        std::cerr << exc.what() << "\n";
        return 1;
    }
    return 0;
}

} // namespace

void log(const std::string& message, const std::string& level) {
    wire().log(level, message);
}

int run_classifier(const std::string& module_id, ClassifyFn classify, ModuleHooks hooks) {
    const bool writable = static_cast<bool>(hooks.on_checkpoint) || static_cast<bool>(hooks.on_restore);
    ExecuteFn on_execute = [classify = std::move(classify)](const Request& req, uint8_t* base,
                                                            Reply& reply) {
        Payload p = read_input(base, req.input);
        reply.classification = classification_json(classify(p, *req.meta));
    };
    return serve(module_id, "classifier", writable, on_execute, hooks);
}

int run_processor(const std::string& module_id, TransformFn transform, ModuleHooks hooks) {
    ExecuteFn on_execute = [transform = std::move(transform)](const Request& req, uint8_t* base,
                                                             Reply& reply) {
        Payload p = read_input(base, req.input);
        std::optional<Output> out = transform(p, *req.meta);
        if (!out) {
            reply.frame = Reply::Frame::Dropped;
            return;
        }
        write_frame(base, req, *out, reply);
    };
    return serve(module_id, "processor", /*writable=*/true, on_execute, hooks);
}

int run_analyzer(const std::string& module_id, AnalyzeFn analyze, ModuleHooks hooks) {
    ExecuteFn on_execute = [analyze = std::move(analyze)](const Request& req, uint8_t* base,
                                                         Reply& reply) {
        Payload p = read_input(base, req.input);
        AnalysisResult out = analyze(p, *req.meta);
        reply.frame = Reply::Frame::Dropped;
        if (out.frame) write_frame(base, req, *out.frame, reply);
        if (out.classification) reply.classification = classification_json(*out.classification);
        if (out.value) reply.value = *out.value;
    };
    return serve(module_id, "analyzer", /*writable=*/true, on_execute, hooks);
}

} // namespace mvf
