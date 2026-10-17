// Bounded Cloud Files sparse Layer provider, quiescent capture, audit and CI helpers.
// Proof fixture only: no product integration, no Git, no host changes outside owned paths.
#define WIN32_LEAN_AND_MEAN
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <bcrypt.h>
#include <cfapi.h>
#include <aclapi.h>
#include <sddl.h>
#include <sstream>
#include <algorithm>
#include <filesystem>
#include <iterator>
#include <fstream>
#include <map>
#include <set>
#include <vector>
#include <mutex>
#include <string>
#include <cstdarg>
#include <cstdio>
#include <cwchar>
#include <cstring>
#include <stdexcept>

namespace fs = std::filesystem;
// Documented ParamSize: offset of the operation member plus its size (SDK 10.0.26100 has no helper macro).
#define PARAM_SIZE(field) static_cast<ULONG>(FIELD_OFFSET(CF_OPERATION_PARAMETERS, field) + RTL_FIELD_SIZE(CF_OPERATION_PARAMETERS, field))
enum Kind { File, Directory, Link };
struct Entry { Kind kind = File; std::wstring source, target; FILE_BASIC_INFO info{}; INT64 size = 0; std::string digest; };
struct NoCase {
    bool operator()(const std::wstring& a, const std::wstring& b) const {
        return CompareStringOrdinal(a.c_str(), -1, b.c_str(), -1, TRUE) == CSTR_LESS_THAN;
    }
};
using View = std::map<std::wstring, Entry, NoCase>;
static std::mutex output;

static bool same(const std::wstring& a, const std::wstring& b) { return CompareStringOrdinal(a.c_str(), -1, b.c_str(), -1, TRUE) == CSTR_EQUAL; }
static bool under(const std::wstring& path, const std::wstring& root) {
    if (root.empty()) return true;
    return path.size() > root.size() && path[root.size()] == L'\\' && same(path.substr(0, root.size()), root);
}
static std::wstring parent(const std::wstring& path) { auto i = path.rfind(L'\\'); return i == std::wstring::npos ? L"" : path.substr(0, i); }
static std::wstring leaf(const std::wstring& path) { auto i = path.rfind(L'\\'); return i == std::wstring::npos ? path : path.substr(i + 1); }
static std::string utf8(const std::wstring& text) {
    int count = WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0, nullptr, nullptr);
    std::string out(static_cast<size_t>(count), '\0');
    WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), out.data(), count, nullptr, nullptr);
    return out;
}
static std::wstring wide(const std::string& text) {
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.data(), static_cast<int>(text.size()), nullptr, 0);
    if (count <= 0 && !text.empty()) throw std::runtime_error("invalid UTF-8 manifest");
    std::wstring out(static_cast<size_t>(count), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), out.data(), count);
    return out;
}
static std::string json(const std::wstring& text) {
    std::string out = "\"";
    for (unsigned char c : utf8(text)) {
        if (c == '"' || c == '\\') { out += '\\'; out += static_cast<char>(c); }
        else if (c < 32) { char buffer[8]; std::snprintf(buffer, sizeof(buffer), "\\u%04x", c); out += buffer; }
        else out += static_cast<char>(c);
    }
    return out + "\"";
}
static void note(const char* format, ...) {
    std::lock_guard lock(output);
    va_list args; va_start(args, format); std::vprintf(format, args); va_end(args);
    std::printf("\n"); std::fflush(stdout);
}
static void require(HRESULT hr, const char* operation) {
    if (FAILED(hr)) { note("%s HRESULT=0x%08lx", operation, static_cast<unsigned long>(hr)); throw std::runtime_error(operation); }
}
template <class T> static void set16(T& field, USHORT value) {
    static_assert(sizeof(T) == sizeof(USHORT)); std::memcpy(&field, &value, sizeof(value));
}
struct Handle {
    HANDLE value;
    explicit Handle(HANDLE h) : value(h) { if (h == INVALID_HANDLE_VALUE) throw std::runtime_error("open bounded object"); }
    ~Handle() { CloseHandle(value); }
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
};
static Handle openObject(const fs::path& path, DWORD access = FILE_READ_ATTRIBUTES | READ_CONTROL) {
    HANDLE h = CreateFileW(path.c_str(), access, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
    if (h == INVALID_HANDLE_VALUE) std::fprintf(stderr, "open-object path=%s error=%lu\n", utf8(path.wstring()).c_str(), GetLastError());
    return Handle(h);
}
static FILE_ATTRIBUTE_TAG_INFO attributes(HANDLE h) {
    FILE_ATTRIBUTE_TAG_INFO t{};
    if (!GetFileInformationByHandleEx(h, FileAttributeTagInfo, &t, sizeof(t))) throw std::runtime_error("object attributes");
    return t;
}
static bool cloud(DWORD tag) { return (tag & ~0x0000f000UL) == IO_REPARSE_TAG_CLOUD; }
static void supported(HANDLE h, bool links = true) {
    auto t = attributes(h);
    if ((t.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && !cloud(t.ReparseTag) && !(links && t.ReparseTag == IO_REPARSE_TAG_SYMLINK))
        throw std::runtime_error("unsupported reparse tag");
}
static void protectedObject(HANDLE h) {
    PSID owner = nullptr; PACL acl = nullptr; PSECURITY_DESCRIPTOR descriptor = nullptr;
    if (GetSecurityInfo(h, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        &owner, nullptr, &acl, nullptr, &descriptor) != ERROR_SUCCESS) throw std::runtime_error("security descriptor");
    auto trusted = [](PSID sid) { return IsWellKnownSid(sid, WinBuiltinAdministratorsSid) || IsWellKnownSid(sid, WinLocalSystemSid); };
    bool ok = owner && trusted(owner) && acl;
    constexpr DWORD writes = GENERIC_ALL | GENERIC_WRITE | WRITE_DAC | WRITE_OWNER | DELETE |
        FILE_DELETE_CHILD | FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES;
    for (DWORD i = 0; ok && i < acl->AceCount; ++i) {
        void* raw = nullptr;
        if (!GetAce(acl, i, &raw)) { ok = false; break; }
        const auto* header = static_cast<ACE_HEADER*>(raw);
        if (header->AceFlags & INHERIT_ONLY_ACE) continue;
        if (header->AceType == ACCESS_DENIED_ACE_TYPE) continue;
        if (header->AceType != ACCESS_ALLOWED_ACE_TYPE) { ok = false; break; }
        auto* ace = static_cast<ACCESS_ALLOWED_ACE*>(raw);
        if ((ace->Mask & writes) && !trusted(&ace->SidStart)) ok = false;
    }
    LocalFree(descriptor);
    if (!ok) throw std::runtime_error("capture requires administrator-protected quiescent objects");
}
static std::vector<char> bytes(const fs::path& path) {
    auto file = openObject(path, GENERIC_READ); supported(file.value, false);
    FILE_STANDARD_INFO before{}, after{};
    if (!GetFileInformationByHandleEx(file.value, FileStandardInfo, &before, sizeof(before)) || before.Directory ||
        before.NumberOfLinks != 1 || before.EndOfFile.QuadPart < 0 || before.EndOfFile.QuadPart > 256 * 1024)
        throw std::runtime_error("bounded file metadata");
    std::vector<char> data(static_cast<size_t>(before.EndOfFile.QuadPart));
    DWORD received = 0;
    if (!ReadFile(file.value, data.data(), static_cast<DWORD>(data.size()), &received, nullptr) || received != data.size())
        throw std::runtime_error("failed or short source read");
    if (!GetFileInformationByHandleEx(file.value, FileStandardInfo, &after, sizeof(after)) || after.EndOfFile.QuadPart != before.EndOfFile.QuadPart)
        throw std::runtime_error("source changed during read");
    return data;
}
static void validPath(const std::wstring& path) {
    if (path.empty() || path.size() > 512 || std::count(path.begin(), path.end(), L'\\') >= 16) throw std::runtime_error("path bound");
    size_t start = 0;
    for (size_t i = 0; i <= path.size(); ++i) {
        if (i < path.size() && (path[i] < 32 || std::wcschr(L"/:*?\"<>|", path[i]))) throw std::runtime_error("invalid path character");
        if (i < path.size() && path[i] != L'\\') continue;
        auto part = path.substr(start, i - start); start = i + 1;
        if (part.empty() || part.back() == L'.' || part.back() == L' ') throw std::runtime_error("path alias");
        auto stem = part.substr(0, part.find(L'.'));
        if (same(stem, L"con") || same(stem, L"prn") || same(stem, L"aux") || same(stem, L"nul") ||
            (stem.size() == 4 && (same(stem.substr(0, 3), L"com") || same(stem.substr(0, 3), L"lpt")) && stem[3] >= L'1' && stem[3] <= L'9'))
            throw std::runtime_error("reserved path");
    }
}
static std::wstring linkTarget(const fs::path& path) {
    auto target = fs::read_symlink(path).wstring(); validPath(target); return target;
}
struct WalkBudget { size_t entries = 0; ULONGLONG started = GetTickCount64(); };
template <class Visit> static void walkAt(const fs::path& root, const std::wstring& relative, Visit& visit, WalkBudget& budget, bool frozen, bool metadataReparses) {
    auto directory = openObject(relative.empty() ? root : root / relative); supported(directory.value, false);
    if (frozen) protectedObject(directory.value);
    WIN32_FIND_DATAW data{};
    HANDLE found = FindFirstFileExW(((relative.empty() ? root : root / relative) / L"*").c_str(), FindExInfoBasic, &data, FindExSearchNameMatch, nullptr, 0);
    if (found == INVALID_HANDLE_VALUE) {
        if (GetLastError() == ERROR_FILE_NOT_FOUND) return;
        throw std::runtime_error("enumerate directory");
    }
    try {
        do {
            std::wstring name = data.cFileName;
            if (name == L"." || name == L"..") continue;
            auto path = relative.empty() ? name : relative + L"\\" + name;
            validPath(path);
            if (++budget.entries > 128 || GetTickCount64() - budget.started > 10000) throw std::runtime_error("walk bound");
            auto object = openObject(root / path); if (!metadataReparses) supported(object.value);
            if (frozen) protectedObject(object.value);
            auto tag = attributes(object.value);
            if (tag.FileAttributes != data.dwFileAttributes || ((tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && tag.ReparseTag != data.dwReserved0))
                throw std::runtime_error("entry changed during walk");
            if (visit(path, data)) walkAt(root, path, visit, budget, frozen, metadataReparses);
        } while (FindNextFileW(found, &data));
        if (GetLastError() != ERROR_NO_MORE_FILES) throw std::runtime_error("incomplete enumeration");
    } catch (...) { FindClose(found); throw; }
    FindClose(found);
}
template <class Visit> static void walk(const fs::path& root, const std::wstring& relative, Visit visit, bool frozen = false, bool metadataReparses = false) {
    WalkBudget budget; walkAt(root, relative, visit, budget, frozen, metadataReparses);
}
static std::wstring objectId(HANDLE h) {
    BY_HANDLE_FILE_INFORMATION info{};
    if (!GetFileInformationByHandle(h, &info)) throw std::runtime_error("object identity");
    return std::to_wstring(info.dwVolumeSerialNumber) + L":" + std::to_wstring(info.nFileIndexHigh) + L":" + std::to_wstring(info.nFileIndexLow);
}
struct RootLease {
    HANDLE mutex;
    explicit RootLease(const fs::path& root) {
        auto h = openObject(root);
        auto name = L"Local\\JavelinSparseProof-" + objectId(h.value);
        mutex = CreateMutexW(nullptr, FALSE, name.c_str());
        if (!mutex) throw std::runtime_error("root lease");
        DWORD result = WaitForSingleObject(mutex, 0);
        if (result != WAIT_OBJECT_0 && result != WAIT_ABANDONED) { CloseHandle(mutex); throw std::runtime_error("root provider or capture is active"); }
    }
    ~RootLease() { ReleaseMutex(mutex); CloseHandle(mutex); }
};
static fs::path bindingPath(const fs::path& root) { return root.parent_path() / L"frozen" / L"bindings" / (root.filename().wstring() + L".txt"); }
static std::wstring normalizedPath(const fs::path& path) { return fs::absolute(path).lexically_normal().wstring(); }
static void ownedRoot(const fs::path& root) {
    wchar_t temporary[32768]{};
    DWORD length = GetEnvironmentVariableW(L"RUNNER_TEMP", temporary, 32768);
    auto container = root.parent_path().filename().wstring();
    const std::wstring prefix = L"javelin-sparse-proof-";
    if (!length || length >= 32768 || !same(normalizedPath(root.parent_path().parent_path()), normalizedPath(temporary)) ||
        container.size() != prefix.size() + 32 || container.substr(0, prefix.size()) != prefix ||
        container.substr(prefix.size()).find_first_not_of(L"0123456789abcdef") != std::wstring::npos)
        throw std::runtime_error("outside owned hosted fixture");
    validPath(root.filename().wstring());
}
static void bindRoot(const fs::path& base, const fs::path& snapshot, const fs::path& root) {
    ownedRoot(root);
    auto container = openObject(bindingPath(root).parent_path()); protectedObject(container.value);
    auto h = openObject(root); supported(h.value, false);
    if (fs::exists(bindingPath(root))) throw std::runtime_error("root already bound");
    std::ofstream out(bindingPath(root), std::ios::binary);
    out << utf8(normalizedPath(root)) << "\n" << utf8(normalizedPath(base)) << "\n" << (snapshot.empty() ? "-" : utf8(normalizedPath(snapshot))) << "\n" << utf8(objectId(h.value)) << "\n";
    out.close(); if (!out) throw std::runtime_error("binding write");
}
static std::wstring approvedRoot(const fs::path& base, const fs::path& root) {
    ownedRoot(root);
    auto frozen = openObject(root.parent_path() / L"frozen"); protectedObject(frozen.value); supported(frozen.value, false);
    auto bindings = openObject(bindingPath(root).parent_path()); protectedObject(bindings.value); supported(bindings.value, false);
    auto record = openObject(bindingPath(root)); protectedObject(record.value); supported(record.value, false);
    auto content = bytes(bindingPath(root)); std::istringstream in(std::string(content.begin(), content.end()));
    std::string r, b, snapshot, id, extra;
    if (!std::getline(in, r) || !std::getline(in, b) || !std::getline(in, snapshot) || !std::getline(in, id) || std::getline(in, extra)) throw std::runtime_error("root binding format");
    auto h = openObject(root); supported(h.value, false);
    if (!same(wide(r), normalizedPath(root)) || !same(wide(b), normalizedPath(base)) || wide(id) != objectId(h.value)) throw std::runtime_error("root binding mismatch");
    return wide(snapshot);
}
static std::string sha256(const std::vector<char>& data) {
    unsigned char digest[32];
    if (BCryptHash(BCRYPT_SHA256_ALG_HANDLE, nullptr, 0, reinterpret_cast<PUCHAR>(const_cast<char*>(data.data())),
        static_cast<ULONG>(data.size()), digest, sizeof(digest)) != 0) throw std::runtime_error("BCryptHash");
    std::string out; char hex[3];
    for (unsigned char c : digest) { std::snprintf(hex, sizeof(hex), "%02x", c); out += hex; }
    return out;
}
static FILE_BASIC_INFO now(DWORD attributes) {
    FILE_BASIC_INFO info{}; FILETIME time; GetSystemTimeAsFileTime(&time);
    std::memcpy(&info.CreationTime, &time, sizeof(time)); info.LastAccessTime = info.LastWriteTime = info.ChangeTime = info.CreationTime;
    info.FileAttributes = attributes; return info;
}
static View scan(const fs::path& base) {
    View view;
    walk(base, L"", [&](const std::wstring& name, const WIN32_FIND_DATAW& data) {
        Entry entry; entry.source = (base / name).wstring();
        if ((data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && data.dwReserved0 == IO_REPARSE_TAG_SYMLINK) { entry.kind = Link; entry.target = linkTarget(entry.source); }
        else if (data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) entry.kind = Directory;
        else { entry.size = (static_cast<INT64>(data.nFileSizeHigh) << 32) | data.nFileSizeLow; entry.digest = sha256(bytes(entry.source)); }
        std::memcpy(&entry.info.CreationTime, &data.ftCreationTime, sizeof(FILETIME));
        std::memcpy(&entry.info.LastAccessTime, &data.ftLastAccessTime, sizeof(FILETIME));
        std::memcpy(&entry.info.LastWriteTime, &data.ftLastWriteTime, sizeof(FILETIME));
        entry.info.ChangeTime = entry.info.LastWriteTime;
        entry.info.FileAttributes = entry.kind == Directory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL;
        view[name] = entry; return entry.kind == Directory;
    });
    return view;
}
// Snapshot manifest: D deleted path, N new directory, B base reference, F private blob, L symlink.
static View load(const fs::path& base, const fs::path& snapshot) {
    auto baseView = scan(base); View view = baseView;
    if (snapshot.empty()) return view;
    auto snapshotRoot = openObject(snapshot); auto blobsRoot = openObject(snapshot / L"blobs");
    if ((attributes(snapshotRoot.value).FileAttributes | attributes(blobsRoot.value).FileAttributes) & FILE_ATTRIBUTE_REPARSE_POINT)
        throw std::runtime_error("snapshot directories must not be reparse points");
    auto content = bytes(snapshot / L"manifest.tsv");
    std::istringstream manifest(std::string(content.begin(), content.end())); std::string raw;
    std::set<std::wstring, NoCase> destinations, deletions; size_t rows = 0; bool writes = false;
    while (std::getline(manifest, raw)) {
        if (++rows > 256) throw std::runtime_error("manifest row bound");
        std::vector<std::wstring> field; size_t start = 0; auto line = wide(raw);
        for (size_t i = 0; i <= line.size(); ++i) if (i == line.size() || line[i] == L'\t') { field.push_back(line.substr(start, i - start)); start = i + 1; }
        if (field.size() < 2 || field[0].size() != 1) throw std::runtime_error("invalid snapshot record");
        auto& path = field[1]; validPath(path);
        if (field[0] == L"D" && field.size() == 2) {
            if (writes || !baseView.count(path) || !deletions.insert(path).second) throw std::runtime_error("ambiguous deletion");
            for (auto it = view.begin(); it != view.end();) it = same(it->first, path) || under(it->first, path) ? view.erase(it) : std::next(it);
            continue;
        }
        writes = true;
        if (!destinations.insert(path).second) throw std::runtime_error("duplicate destination");
        if (field[0] == L"N" && field.size() == 2) { Entry entry; entry.kind = Directory; entry.info = now(FILE_ATTRIBUTE_DIRECTORY); view[path] = entry; }
        else if (field.size() == 3 && field[0] == L"B") {
            validPath(field[2]); auto original = baseView.find(field[2]);
            if (original == baseView.end() || original->second.kind != File) throw std::runtime_error("invalid base reference");
            view[path] = original->second;
        } else if (field.size() == 3 && field[0] == L"F") {
            const auto& digest = field[2];
            if (digest.size() != 64 || digest.find_first_not_of(L"0123456789abcdef") != std::wstring::npos) throw std::runtime_error("invalid blob digest name");
            Entry entry; entry.source = (snapshot / L"blobs" / digest).wstring(); auto data = bytes(entry.source);
            entry.digest = sha256(data);
            if (entry.digest != utf8(digest)) throw std::runtime_error("blob digest mismatch");
            entry.size = static_cast<INT64>(data.size()); entry.info = now(FILE_ATTRIBUTE_NORMAL); view[path] = entry;
        } else if (field.size() == 3 && field[0] == L"L") {
            validPath(field[2]); Entry entry; entry.kind = Link; entry.target = field[2]; view[path] = entry;
        } else throw std::runtime_error("unknown snapshot record");
        if (view.size() > 128) throw std::runtime_error("view exceeds 128 entries");
    }
    for (const auto& [path, entry] : view) {
        auto p = parent(path);
        if (!p.empty() && (!view.count(p) || view.at(p).kind != Directory)) throw std::runtime_error("view prefix conflict");
        if (entry.kind == Link && (!p.empty() || !view.count(entry.target) || view.at(entry.target).kind != File))
            throw std::runtime_error("link target must name a declared file from the root");
    }
    return view;
}

struct Provider {
    View view; fs::path root, slow; CF_CONNECTION_KEY connection{};
    static Provider* self;
    static std::wstring identity(const CF_CALLBACK_INFO* info) {
        return std::wstring(static_cast<const wchar_t*>(info->FileIdentity), info->FileIdentityLength / sizeof(wchar_t));
    }
    static CF_OPERATION_INFO operation(const CF_CALLBACK_INFO* info, CF_OPERATION_TYPE type) {
        CF_OPERATION_INFO op{}; op.StructSize = sizeof(op); op.Type = type; op.ConnectionKey = info->ConnectionKey;
        op.TransferKey = info->TransferKey; op.RequestKey = info->RequestKey; return op;
    }
    static void CALLBACK fetchData(const CF_CALLBACK_INFO* info, const CF_CALLBACK_PARAMETERS* parameters) {
        auto key = identity(info); auto found = self->view.find(key);
        note("fetch-data key=%s source=%s flags=%lu required=%lld+%lld", utf8(key).c_str(),
            found == self->view.end() ? "-" : utf8(found->second.source).c_str(), static_cast<unsigned long>(parameters->FetchData.Flags),
            parameters->FetchData.RequiredFileOffset.QuadPart, parameters->FetchData.RequiredLength.QuadPart);
        std::vector<char> data; NTSTATUS status = STATUS_SUCCESS;
        try {
            if (found == self->view.end() || found->second.kind != File) throw std::runtime_error("no file entry");
            if (same(leaf(key), L"slow.bin") && fs::exists(self->slow)) { note("fetch-slow-wait key=%s", utf8(key).c_str()); Sleep(45000); }
            data = bytes(found->second.source);
            if (static_cast<INT64>(data.size()) != found->second.size || sha256(data) != found->second.digest) throw std::runtime_error("immutable source size changed");
        } catch (const std::exception& error) { note("fetch-data-error %s", error.what()); status = STATUS_CLOUD_FILE_UNSUCCESSFUL; }
        auto op = operation(info, CF_OPERATION_TYPE_TRANSFER_DATA);
        CF_OPERATION_PARAMETERS p{}; p.ParamSize = PARAM_SIZE(TransferData); p.TransferData.CompletionStatus = status;
        if (status == STATUS_SUCCESS) { p.TransferData.Buffer = data.data(); p.TransferData.Offset.QuadPart = 0; p.TransferData.Length.QuadPart = static_cast<LONGLONG>(data.size()); }
        else { p.TransferData.Offset = parameters->FetchData.RequiredFileOffset; p.TransferData.Length = parameters->FetchData.RequiredLength; }
        HRESULT hr = CfExecute(&op, &p);
        note("transfer-data key=%s bytes=%zu hr=%08lx", utf8(key).c_str(), data.size(), static_cast<unsigned long>(hr));
    }
    static void CALLBACK fetchPlaceholders(const CF_CALLBACK_INFO* info, const CF_CALLBACK_PARAMETERS*) {
        auto key = identity(info);
        std::vector<std::wstring> names, ids; std::vector<const Entry*> children; NTSTATUS status = STATUS_SUCCESS;
        for (const auto& [path, entry] : self->view) {
            if (!same(parent(path), key) || path.empty()) continue;
            if (entry.kind == Link) status = STATUS_CLOUD_FILE_NOT_SUPPORTED; // Fail closed; only root symlinks are bounded here.
            names.push_back(leaf(path)); ids.push_back(path); children.push_back(&entry);
        }
        std::vector<CF_PLACEHOLDER_CREATE_INFO> create(children.size());
        for (size_t i = 0; i < children.size(); ++i) {
            create[i].RelativeFileName = names[i].c_str(); create[i].FsMetadata.BasicInfo = children[i]->info;
            create[i].FsMetadata.FileSize.QuadPart = children[i]->size; create[i].FileIdentity = ids[i].c_str();
            create[i].FileIdentityLength = static_cast<DWORD>(ids[i].size() * sizeof(wchar_t)); create[i].Flags = CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC;
        }
        auto op = operation(info, CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS);
        CF_OPERATION_PARAMETERS p{}; p.ParamSize = PARAM_SIZE(TransferPlaceholders);
        p.TransferPlaceholders.Flags = CF_OPERATION_TRANSFER_PLACEHOLDERS_FLAG_DISABLE_ON_DEMAND_POPULATION;
        p.TransferPlaceholders.CompletionStatus = status;
        if (status == STATUS_SUCCESS) {
            p.TransferPlaceholders.PlaceholderTotalCount.QuadPart = static_cast<LONGLONG>(create.size());
            p.TransferPlaceholders.PlaceholderArray = create.empty() ? nullptr : create.data();
            p.TransferPlaceholders.PlaceholderCount = static_cast<DWORD>(create.size());
        }
        HRESULT hr = CfExecute(&op, &p);
        note("transfer-placeholders key=%s path=%s count=%zu status=%08lx hr=%08lx processed=%lu", utf8(key).c_str(), utf8(info->NormalizedPath).c_str(),
            create.size(), static_cast<unsigned long>(status), static_cast<unsigned long>(hr), static_cast<unsigned long>(p.TransferPlaceholders.EntriesProcessed));
    }
    static void CALLBACK notifyRename(const CF_CALLBACK_INFO* info, const CF_CALLBACK_PARAMETERS* parameters) {
        auto op = operation(info, CF_OPERATION_TYPE_ACK_RENAME);
        CF_OPERATION_PARAMETERS p{}; p.ParamSize = PARAM_SIZE(AckRename); p.AckRename.CompletionStatus = STATUS_SUCCESS;
        HRESULT hr = CfExecute(&op, &p);
        note("notify-rename key=%s from=%s to=%s flags=%lu ack=%08lx", utf8(identity(info)).c_str(), utf8(info->NormalizedPath).c_str(),
            utf8(parameters->Rename.TargetPath).c_str(), static_cast<unsigned long>(parameters->Rename.Flags), static_cast<unsigned long>(hr));
    }
    static void CALLBACK notifyDelete(const CF_CALLBACK_INFO* info, const CF_CALLBACK_PARAMETERS*) {
        auto op = operation(info, CF_OPERATION_TYPE_ACK_DELETE);
        CF_OPERATION_PARAMETERS p{}; p.ParamSize = PARAM_SIZE(AckDelete); p.AckDelete.CompletionStatus = STATUS_SUCCESS;
        HRESULT hr = CfExecute(&op, &p);
        note("notify-delete key=%s path=%s ack=%08lx", utf8(identity(info)).c_str(), utf8(info->NormalizedPath).c_str(), static_cast<unsigned long>(hr));
    }
    int serve(const fs::path& base, const fs::path& snapshot, const fs::path& owned, const fs::path& stop, const fs::path& ready) {
        if (!same(approvedRoot(base, owned), snapshot.empty() ? L"-" : normalizedPath(snapshot))) throw std::runtime_error("provider binding mode mismatch");
        RootLease lease(owned);
        root = owned; slow = root.wstring() + L".slow"; view = load(base, snapshot); self = this;
        auto marker = root.wstring() + L".registered";
        std::vector<BYTE> registrationInfo(sizeof(CF_SYNC_ROOT_STANDARD_INFO) + 4096); DWORD registrationSize = 0;
        HRESULT registered = CfGetSyncRootInfoByPath(root.c_str(), CF_SYNC_ROOT_INFO_STANDARD, registrationInfo.data(), static_cast<DWORD>(registrationInfo.size()), &registrationSize);
        if (FAILED(registered)) {
            fs::create_directory(root);
            std::wstring syncIdentity = L"javelin-sparse-proof";
            CF_SYNC_REGISTRATION registration{}; registration.StructSize = sizeof(registration);
            registration.ProviderName = L"JavelinSparseProof"; registration.ProviderVersion = L"0.1";
            registration.SyncRootIdentity = syncIdentity.c_str(); registration.SyncRootIdentityLength = static_cast<DWORD>(syncIdentity.size() * sizeof(wchar_t));
            registration.ProviderId = { 0x6a7e1f20, 0x3c4b, 0x4d1e, { 0x9a, 0x51, 0x2b, 0x6f, 0x0d, 0x83, 0x14, 0xc7 } };
            CF_SYNC_POLICIES policies{}; policies.StructSize = sizeof(policies);
            set16(policies.Hydration.Primary, static_cast<USHORT>(CF_HYDRATION_POLICY_FULL)); set16(policies.Population.Primary, static_cast<USHORT>(CF_POPULATION_POLICY_FULL));
            policies.InSync = CF_INSYNC_POLICY_TRACK_ALL; policies.HardLink = CF_HARDLINK_POLICY_NONE;
            require(CfRegisterSyncRoot(root.c_str(), &registration, &policies, CF_REGISTER_FLAG_DISABLE_ON_DEMAND_POPULATION_ON_ROOT), "CfRegisterSyncRoot");
            // The root is populated once while inactive: placeholders for files/directories, real symlinks for links.
            std::vector<std::wstring> names, ids; std::vector<const Entry*> children;
            for (const auto& [path, entry] : view) {
                if (path.empty() || !parent(path).empty()) continue;
                if (entry.kind == Link) {
                    auto link = (root / path).wstring();
                    if (!CreateSymbolicLinkW(link.c_str(), entry.target.c_str(), SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE))
                        { note("CreateSymbolicLinkW error=%lu", GetLastError()); throw std::runtime_error("root symlink"); }
                    continue;
                }
                names.push_back(path); ids.push_back(path); children.push_back(&entry);
            }
            std::vector<CF_PLACEHOLDER_CREATE_INFO> create(children.size());
            for (size_t i = 0; i < children.size(); ++i) {
                create[i].RelativeFileName = names[i].c_str(); create[i].FsMetadata.BasicInfo = children[i]->info;
                create[i].FsMetadata.FileSize.QuadPart = children[i]->size; create[i].FileIdentity = ids[i].c_str();
                create[i].FileIdentityLength = static_cast<DWORD>(ids[i].size() * sizeof(wchar_t)); create[i].Flags = CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC;
            }
            DWORD processed = 0;
            if (!create.empty()) require(CfCreatePlaceholders(root.c_str(), create.data(), static_cast<DWORD>(create.size()), CF_CREATE_FLAG_NONE, &processed), "CfCreatePlaceholders");
            note("registered root=%s rootPlaceholders=%lu", utf8(root.wstring()).c_str(), static_cast<unsigned long>(processed));
            std::ofstream(fs::path(marker)) << "snapshot=" << (snapshot.empty() ? std::string("-") : utf8(snapshot.wstring()));
        }
        CF_CALLBACK_REGISTRATION table[] = {
            { CF_CALLBACK_TYPE_FETCH_DATA, fetchData }, { CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS, fetchPlaceholders },
            { CF_CALLBACK_TYPE_NOTIFY_RENAME, notifyRename }, { CF_CALLBACK_TYPE_NOTIFY_DELETE, notifyDelete }, CF_CALLBACK_REGISTRATION_END };
        require(CfConnectSyncRoot(root.c_str(), table, this, CF_CONNECT_FLAG_REQUIRE_FULL_FILE_PATH, &connection), "CfConnectSyncRoot");
        note("started root=%s entries=%zu", utf8(root.wstring()).c_str(), view.size());
        std::ofstream(ready) << "connected";
        ULONGLONG deadline = GetTickCount64() + 240000;
        while (!fs::exists(stop) && GetTickCount64() < deadline) Sleep(50);
        require(CfDisconnectSyncRoot(connection), "CfDisconnectSyncRoot");
        note("stopped root=%s", utf8(root.wstring()).c_str());
        return fs::exists(stop) ? 0 : 124;
    }
};
Provider* Provider::self = nullptr;

static void expose() {
    using Mode = CHAR (NTAPI*)(CHAR);
    auto set = reinterpret_cast<Mode>(GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "RtlSetProcessPlaceholderCompatibilityMode"));
    if (!set) throw std::runtime_error("placeholder compatibility API missing");
    set(2); // PHCM_EXPOSE_PLACEHOLDERS: this process inspects cloud reparse state instead of hiding it.
}
// Hosted administrator fixture operation. Inspect reparses themselves, never their targets.
static void rootAccess(const fs::path& root, const std::wstring& sid, bool seal) {
    ownedRoot(root); expose();
    const std::wstring inherit = seal ? L"" : L"OICI";
    std::wstring sddl = L"O:BAD:P(A;" + inherit + L";FA;;;BA)(A;" + inherit + L";FA;;;SY)(A;" + inherit + L";" + std::wstring(seal ? L"FRFX" : L"FA") + L";;;" + sid + L")";
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1, &descriptor, nullptr)) throw std::runtime_error("fixture access descriptor");
    PSID owner = nullptr; BOOL defaulted = FALSE, present = FALSE; PACL acl = nullptr;
    GetSecurityDescriptorOwner(descriptor, &owner, &defaulted); GetSecurityDescriptorDacl(descriptor, &present, &acl, &defaulted);
    auto apply = [&](const fs::path& path) {
        auto h = openObject(path, FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC | WRITE_OWNER);
        if (SetSecurityInfo(h.value, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            owner, nullptr, acl, nullptr) != ERROR_SUCCESS) throw std::runtime_error("fixture access change");
    };
    try {
        apply(root);
        walk(root, L"", [&](const std::wstring& path, const WIN32_FIND_DATAW& data) {
            apply(root / path);
            auto state = CfGetPlaceholderStateFromFindData(&data);
            bool reparse = (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
            return (data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) && (!reparse || cloud(data.dwReserved0)) &&
                !(state != CF_PLACEHOLDER_STATE_INVALID && (state & CF_PLACEHOLDER_STATE_PARTIAL));
        }, false, true);
    } catch (...) { LocalFree(descriptor); throw; }
    LocalFree(descriptor);
}
struct Local { Kind kind; std::wstring base, blob, target; CF_PLACEHOLDER_STATE state; };
static std::wstring placeholderIdentity(const fs::path& path) {
    HANDLE handle = CreateFileW(path.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    if (handle == INVALID_HANDLE_VALUE) throw std::runtime_error("open placeholder");
    std::vector<BYTE> buffer(sizeof(CF_PLACEHOLDER_BASIC_INFO) + 4096); DWORD returned = 0;
    HRESULT hr = CfGetPlaceholderInfo(handle, CF_PLACEHOLDER_INFO_BASIC, buffer.data(), static_cast<DWORD>(buffer.size()), &returned);
    CloseHandle(handle); require(hr, "CfGetPlaceholderInfo");
    auto info = reinterpret_cast<CF_PLACEHOLDER_BASIC_INFO*>(buffer.data());
    if (returned < FIELD_OFFSET(CF_PLACEHOLDER_BASIC_INFO, FileIdentity) ||
        info->FileIdentityLength > returned - FIELD_OFFSET(CF_PLACEHOLDER_BASIC_INFO, FileIdentity) || info->FileIdentityLength % sizeof(wchar_t))
        throw std::runtime_error("invalid placeholder identity length");
    return std::wstring(reinterpret_cast<wchar_t*>(info->FileIdentity), info->FileIdentityLength / sizeof(wchar_t));
}
// Quiescent capture: the provider is stopped and partial directories are never enumerated.
static int capture(const fs::path& base, const fs::path& root, const fs::path& out) {
    if (approvedRoot(base, root) != L"-") throw std::runtime_error("capture supports base-served roots only");
    auto frozenRoot = openObject(root); supported(frozenRoot.value, false); protectedObject(frozenRoot.value);
    RootLease lease(root);
    if (fs::exists(out) || fs::exists(fs::path(out.wstring() + L".pending"))) throw std::runtime_error("capture output or staging already exists");
    expose();
    auto baseView = scan(base); std::map<std::wstring, Local, NoCase> local;
    std::map<std::wstring, std::vector<char>> privateBlobs;
    walk(root, L"", [&](const std::wstring& path, const WIN32_FIND_DATAW& data) {
        auto full = root / path; auto state = CfGetPlaceholderStateFromFindData(&data);
        bool directory = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY;
        if ((data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && data.dwReserved0 == IO_REPARSE_TAG_SYMLINK) {
            local[path] = { Link, L"", L"", linkTarget(full), state }; return false;
        }
        if (state != CF_PLACEHOLDER_STATE_INVALID && (state & CF_PLACEHOLDER_STATE_PLACEHOLDER)) {
            auto id = placeholderIdentity(full);
            auto original = baseView.find(id);
            if (original == baseView.end() || (directory ? original->second.kind != Directory : original->second.kind != File)) throw std::runtime_error("invalid placeholder identity");
            if (directory && (state & CF_PLACEHOLDER_STATE_PARTIAL)) {
                for (const auto& [name, entry] : baseView)
                    if (same(name, id) || under(name, id)) local[path + name.substr(id.size())] = { entry.kind, name, L"", entry.target, state };
                return false;
            }
            if (directory) { local[path] = { Directory, id, L"", L"", state }; return true; }
            INT64 size = (static_cast<INT64>(data.nFileSizeHigh) << 32) | data.nFileSizeLow;
            if (state & CF_PLACEHOLDER_STATE_PARTIAL) {
                auto h = openObject(full);
                std::vector<BYTE> infoBuffer(sizeof(CF_PLACEHOLDER_STANDARD_INFO) + 4096); DWORD returned = 0;
                require(CfGetPlaceholderInfo(h.value, CF_PLACEHOLDER_INFO_STANDARD, infoBuffer.data(), static_cast<DWORD>(infoBuffer.size()), &returned), "partial metadata");
                if (returned < FIELD_OFFSET(CF_PLACEHOLDER_STANDARD_INFO, FileIdentity)) throw std::runtime_error("short partial metadata");
                auto* info = reinterpret_cast<CF_PLACEHOLDER_STANDARD_INFO*>(infoBuffer.data());
                if (size != original->second.size || info->OnDiskDataSize.QuadPart != 0 || info->ModifiedDataSize.QuadPart != 0)
                    throw std::runtime_error("uncertain partial file cannot be captured");
                local[path] = { File, id, L"", L"", state }; return false;
            }
            if (size == original->second.size && (state & CF_PLACEHOLDER_STATE_IN_SYNC)) { local[path] = { File, id, L"", L"", state }; return false; }
            if (size == original->second.size && sha256(bytes(full)) == original->second.digest) {
                local[path] = { File, id, L"", L"", state }; return false;
            }
        } else if (directory) { local[path] = { Directory, L"", L"", L"", state }; return true; }
        auto content = bytes(full); auto digest = sha256(content);
        privateBlobs[wide(digest)] = std::move(content);
        local[path] = { File, L"", wide(digest), L"", state }; return false;
    }, true);
    if (local.size() > 128) throw std::runtime_error("expanded capture entry bound");
    const fs::path pending(out.wstring() + L".pending");
    fs::create_directories(pending / L"blobs");
    struct Staging { fs::path path; ~Staging() { std::error_code error; fs::remove_all(path, error); } } staging{pending};
    for (const auto& [digest, data] : privateBlobs) {
        std::ofstream blob(pending / L"blobs" / digest, std::ios::binary);
        blob.write(data.data(), static_cast<std::streamsize>(data.size())); blob.close();
        if (!blob) throw std::runtime_error("snapshot blob write failed");
    }
    std::ofstream manifest(pending / L"manifest.tsv", std::ios::binary); std::set<std::wstring, NoCase> deleted;
    size_t copies = 0, references = 0, deletions = 0;
    for (const auto& [path, entry] : baseView) {
        auto found = local.find(path);
        bool keep = found != local.end() && found->second.kind == entry.kind;
        if (keep) continue;
        bool covered = false; for (const auto& d : deleted) covered |= under(path, d);
        if (!covered) { deleted.insert(path); manifest << "D\t" << utf8(path) << "\n"; ++deletions; }
    }
    for (const auto& [path, entry] : local) {
        auto original = baseView.find(path);
        bool baseHas = original != baseView.end() && original->second.kind == entry.kind;
        if (entry.kind == Directory) { if (!baseHas) manifest << "N\t" << utf8(path) << "\n"; continue; }
        if (entry.kind == Link) { if (!baseHas || original->second.target != entry.target) manifest << "L\t" << utf8(path) << "\t" << utf8(entry.target) << "\n"; ++references; continue; }
        if (!entry.base.empty()) { ++references; if (!baseHas || !same(entry.base, path)) manifest << "B\t" << utf8(path) << "\t" << utf8(entry.base) << "\n"; continue; }
        ++copies; manifest << "F\t" << utf8(path) << "\t" << utf8(entry.blob) << "\n";
    }
    manifest.close();
    if (!manifest) throw std::runtime_error("snapshot manifest write failed");
    (void)load(base, pending);
    fs::rename(pending, out);
    std::printf("{\"privateCopies\":%zu,\"references\":%zu,\"deletions\":%zu,\"entries\":%zu}\n", copies, references, deletions, local.size());
    return 0;
}
static int audit(const fs::path& root) {
    expose();
    std::printf("{\"entries\":[");
    bool first = true;
    walk(root, L"", [&](const std::wstring& path, const WIN32_FIND_DATAW& data) {
        auto state = CfGetPlaceholderStateFromFindData(&data);
        INT64 logical = 0, allocated = 0; DWORD error = 0;
        HANDLE handle = CreateFileW((root / path).c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
        if (handle != INVALID_HANDLE_VALUE) {
            FILE_STANDARD_INFO info{};
            if (GetFileInformationByHandleEx(handle, FileStandardInfo, &info, sizeof(info))) { logical = info.EndOfFile.QuadPart; allocated = info.AllocationSize.QuadPart; }
            else error = GetLastError();
            CloseHandle(handle);
        } else error = GetLastError();
        std::printf("%s{\"path\":%s,\"attributes\":%lu,\"reparseTag\":%lu,\"placeholderState\":%lu,\"logicalBytes\":%lld,\"allocatedBytes\":%lld,\"error\":%lu}",
            first ? "" : ",", json(path).c_str(), static_cast<unsigned long>(data.dwFileAttributes),
            static_cast<unsigned long>(data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT ? data.dwReserved0 : 0),
            static_cast<unsigned long>(state), logical, allocated, static_cast<unsigned long>(error));
        first = false;
        bool directory = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY;
        bool partial = state != CF_PLACEHOLDER_STATE_INVALID && (state & CF_PLACEHOLDER_STATE_PARTIAL);
        bool link = (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && data.dwReserved0 == IO_REPARSE_TAG_SYMLINK;
        return directory && !partial && !link;
    });
    std::printf("]}\n"); return 0;
}
static int result(const char* operation, BOOL ok) {
    DWORD error = ok ? 0 : GetLastError();
    std::printf("{\"operation\":\"%s\",\"ok\":%s,\"win32\":%lu}\n", operation, ok ? "true" : "false", static_cast<unsigned long>(error));
    return ok ? 0 : 1;
}
// Holds an open write handle after a partial write so the caller can kill this process mid-write.
static int hold(const fs::path& path, const std::wstring& mode, const std::wstring& text, const fs::path& ready) {
    HANDLE file = CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, mode == L"create" ? CREATE_ALWAYS : OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return result("hold-open", FALSE);
    auto data = utf8(text); DWORD written = 0;
    if (!WriteFile(file, data.data(), static_cast<DWORD>(data.size()), &written, nullptr) || written != data.size()) return result("hold-write", FALSE);
    std::ofstream(ready) << written;
    Sleep(INFINITE); return 0;
}
static int flushDirectory(const fs::path& path) {
    HANDLE directory = CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    if (directory == INVALID_HANDLE_VALUE) return result("open-directory-for-flush", FALSE);
    BOOL ok = FlushFileBuffers(directory); int code = result("flush-directory", ok); CloseHandle(directory); return code;
}

static std::wstring quote(const std::wstring& argument) {
    std::wstring out = L"\""; size_t slashes = 0;
    for (wchar_t c : argument) {
        if (c == L'\\') { ++slashes; continue; }
        out.append(slashes * (c == L'"' ? 2 : 1), L'\\'); slashes = 0;
        if (c == L'"') out += L'\\'; out += c;
    }
    out.append(slashes * 2, L'\\'); return out + L'"';
}
// Same suspended-assign-resume Windows job wrapper as the PR #9 proof, pinned here for independent use.
static int bounded(int argc, wchar_t** argv) {
    DWORD milliseconds = static_cast<DWORD>(std::stoul(argv[2]));
    if (milliseconds > 200000 || milliseconds < 100) return 2;
    std::wstring command;
    for (int i = 3; i < argc; ++i) { if (i > 3) command += L' '; command += quote(argv[i]); }
    HANDLE job = CreateJobObjectW(nullptr, nullptr);
    if (!job) return 2;
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{}; limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) { CloseHandle(job); return 2; }
    STARTUPINFOW startup{}; startup.cb = sizeof(startup); startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE); startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE); startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION child{};
    if (!CreateProcessW(nullptr, command.data(), nullptr, nullptr, TRUE, CREATE_SUSPENDED, nullptr, nullptr, &startup, &child)) {
        std::printf("CreateProcessW error=%lu\n", GetLastError()); CloseHandle(job); return 2;
    }
    if (!AssignProcessToJobObject(job, child.hProcess)) {
        std::printf("AssignProcessToJobObject error=%lu\n", GetLastError()); TerminateProcess(child.hProcess, 2);
        CloseHandle(child.hThread); CloseHandle(child.hProcess); CloseHandle(job); return 2;
    }
    ResumeThread(child.hThread); CloseHandle(child.hThread);
    bool timedOut = WaitForSingleObject(child.hProcess, milliseconds) != WAIT_OBJECT_0;
    DWORD code = 124; if (!timedOut) GetExitCodeProcess(child.hProcess, &code);
    TerminateJobObject(job, 124);
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting{};
    ULONGLONG deadline = GetTickCount64() + 5000; bool queried = false;
    do {
        queried = QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), nullptr) != FALSE;
        if (!queried || accounting.ActiveProcesses == 0) break; Sleep(20);
    } while (GetTickCount64() < deadline);
    std::fprintf(stderr, "WINDOWS_JOB parent=%lu timedOut=%d exit=%lu activeAfterTermination=%lu\n", child.dwProcessId, timedOut,
        code, accounting.ActiveProcesses);
    CloseHandle(child.hProcess); CloseHandle(job);
    return queried && accounting.ActiveProcesses == 0 ? static_cast<int>(code) : 125;
}

int wmain(int argc, wchar_t** argv) {
    try {
        wchar_t hosted[32]{}, actions[16]{};
        GetEnvironmentVariableW(L"RUNNER_ENVIRONMENT", hosted, 32); GetEnvironmentVariableW(L"GITHUB_ACTIONS", actions, 16);
        if (std::wcscmp(hosted, L"github-hosted") != 0 || std::wcscmp(actions, L"true") != 0) return 2;
        std::wstring verb = argc > 1 ? argv[1] : L"";
        if (argc >= 4 && verb == L"bounded") return bounded(argc, argv);
        if (argc == 7 && verb == L"serve") { Provider provider; return provider.serve(argv[2], std::wcscmp(argv[3], L"-") ? fs::path(argv[3]) : fs::path(), argv[4], argv[5], argv[6]); }
        if (argc == 5 && verb == L"access") { rootAccess(argv[2], argv[3], std::wcscmp(argv[4], L"seal") == 0); return 0; }
        if (argc == 5 && verb == L"bind") { bindRoot(argv[2], std::wcscmp(argv[3], L"-") ? fs::path(argv[3]) : fs::path(), argv[4]); return 0; }
        if (argc == 3 && verb == L"validate") { auto h = openObject(argv[2]); protectedObject(h.value); return 0; }
        if (argc == 5 && verb == L"capture") return capture(argv[2], argv[3], argv[4]);
        if (argc == 3 && verb == L"audit") return audit(argv[2]);
        if (argc == 3 && verb == L"unregister") { HRESULT hr = CfUnregisterSyncRoot(argv[2]); std::printf("{\"unregister\":\"%08lx\"}\n", static_cast<unsigned long>(hr)); return FAILED(hr); }
        if (argc == 4 && verb == L"move") return result("move", MoveFileExW(argv[2], argv[3], 0));
        if (argc == 6 && verb == L"hold") return hold(argv[2], argv[3], argv[4], argv[5]);
        if (argc == 3 && verb == L"flush-directory") return flushDirectory(argv[2]);
        return 2;
    } catch (const std::exception& error) { std::fprintf(stderr, "sparse proof error: %s\n", error.what()); return 2; }
}
