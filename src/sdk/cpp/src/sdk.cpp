// MVF C++ module SDK implementation. Mirrors src/sdk/python/mvf_sdk/__init__.py:
// the same stdio control loop, the same 192-byte typed descriptor, the same
// shared-memory arena addressed by MVF_ARENA_PATH. Little-endian hosts only
// (x86-64 / arm64), matching the on-wire descriptor layout.

#include "mvf/sdk.hpp"

#include <cstring>
#include <cstdlib>
#include <iostream>
#include <stdexcept>

#if defined(_WIN32)
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

// ---- stdio control plane ----

void send(const json& obj) {
    std::cout << obj.dump() << "\n";
    std::cout.flush();
}

Payload read_input(uint8_t* base, const json& frame) {
    if (!frame.contains("shm") || !frame["shm"].contains("offset"))
        throw std::runtime_error("frame is missing its shared-memory handle");
    return read_descriptor(base, frame["shm"]["offset"].get<int64_t>());
}

json optional_json(const std::optional<double>& v) { return v ? json(*v) : json(nullptr); }
json optional_json(const std::optional<std::string>& v) { return v ? json(*v) : json(nullptr); }

using ExecuteFn = std::function<void(const json& msg, const json& id, uint8_t* base)>;

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

    if (hooks.on_start) {
        send({{"type", "hello"}, {"protocol", 1}, {"moduleId", module_id},
              {"capability", capability}, {"ready", false}});
        hooks.on_start();
        send({{"type", "ready"}, {"moduleId", module_id}});
    } else {
        send({{"type", "hello"}, {"protocol", 1}, {"moduleId", module_id},
              {"capability", capability}});
    }

    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) continue;
        json msg = json::parse(line, nullptr, /*allow_exceptions=*/false);
        if (msg.is_discarded()) continue;
        const std::string type = msg.value("type", "");
        if (type == "shutdown") break;

        const json id = msg.contains("id") ? msg["id"] : json(nullptr);
        try {
            if (type == "execute") {
                on_execute(msg, id, base);
            } else if (type == "checkpoint") {
                std::optional<Output> state = hooks.on_checkpoint ? hooks.on_checkpoint()
                                                                  : std::nullopt;
                if (!state) {
                    send({{"type", "state"}, {"id", id}, {"empty", true}});
                } else {
                    const json& out = msg.at("out");
                    write_descriptor(base, out["offset"].get<int64_t>(), *state,
                                     out["capacity"].get<int64_t>());
                    send({{"type", "state"}, {"id", id}});
                }
            } else if (type == "restore") {
                Payload p = read_input(base, msg);
                if (hooks.on_restore) hooks.on_restore(p);
                send({{"type", "restored"}, {"id", id}});
            }
        } catch (const std::exception& exc) {
            send({{"type", "error"}, {"id", id}, {"message", exc.what()}});
        }
    }
    return 0;
}

} // namespace

int run_classifier(const std::string& module_id, ClassifyFn classify, ModuleHooks hooks) {
    const bool writable = static_cast<bool>(hooks.on_checkpoint) || static_cast<bool>(hooks.on_restore);
    ExecuteFn on_execute = [classify = std::move(classify)](const json& msg, const json& id,
                                                            uint8_t* base) {
        const json frame = msg.contains("frame") ? msg["frame"] : json::object();
        Payload p = read_input(base, frame);
        Classification c = classify(p, frame);
        json classification = {
            {"label", c.label},
            {"measurement", optional_json(c.measurement)},
            {"unit", optional_json(c.unit)},
            {"details", optional_json(c.details)},
        };
        send({{"type", "result"}, {"id", id}, {"classification", classification}});
    };
    return serve(module_id, "classifier", writable, on_execute, hooks);
}

int run_processor(const std::string& module_id, TransformFn transform, ModuleHooks hooks) {
    ExecuteFn on_execute = [transform = std::move(transform)](const json& msg, const json& id,
                                                             uint8_t* base) {
        const json frame = msg.contains("frame") ? msg["frame"] : json::object();
        Payload p = read_input(base, frame);
        std::optional<Output> out = transform(p, frame);
        if (!out) {
            send({{"type", "result"}, {"id", id}, {"frame", nullptr}});
            return;
        }
        const json& slot = msg.at("out");
        const int64_t offset = slot["offset"].get<int64_t>();
        write_descriptor(base, offset, *out, slot["capacity"].get<int64_t>());
        send({{"type", "result"}, {"id", id}, {"frame", {{"shm", {{"offset", offset}}}}}});
    };
    return serve(module_id, "processor", /*writable=*/true, on_execute, hooks);
}

} // namespace mvf
