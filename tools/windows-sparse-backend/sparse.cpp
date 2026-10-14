// Bounded Cloud Files sparse Layer provider, quiescent capture, audit and CI helpers.
// Proof fixture only: no product integration, no Git, no host changes outside owned paths.
#define WIN32_LEAN_AND_MEAN
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <bcrypt.h>
#include <cfapi.h>
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
struct Entry { Kind kind = File; std::wstring source, target; FILE_BASIC_INFO info{}; INT64 size = 0; };
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
static std::vector<char> bytes(const fs::path& path) {
    std::ifstream input(path, std::ios::binary);
    if (!input) throw std::runtime_error("cannot read bounded source file");
    return std::vector<char>(std::istreambuf_iterator<char>(input), {});
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
    for (const auto& item : fs::recursive_directory_iterator(base)) {
        Entry entry; auto name = item.path().lexically_relative(base).wstring();
        entry.source = item.path().wstring();
        if (item.is_symlink()) { entry.kind = Link; entry.target = fs::read_symlink(item.path()).wstring(); }
        else if (item.is_directory()) entry.kind = Directory;
        else {
            entry.size = static_cast<INT64>(item.file_size());
            if (entry.size > 256 * 1024) throw std::runtime_error("fixture file exceeds 256 KiB bound");
        }
        WIN32_FILE_ATTRIBUTE_DATA data{};
        if (!GetFileAttributesExW(entry.source.c_str(), GetFileExInfoStandard, &data)) throw std::runtime_error("base metadata");
        std::memcpy(&entry.info.CreationTime, &data.ftCreationTime, sizeof(FILETIME));
        std::memcpy(&entry.info.LastAccessTime, &data.ftLastAccessTime, sizeof(FILETIME));
        std::memcpy(&entry.info.LastWriteTime, &data.ftLastWriteTime, sizeof(FILETIME));
        entry.info.ChangeTime = entry.info.LastWriteTime;
        entry.info.FileAttributes = entry.kind == Directory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL;
        view[name] = entry;
        if (view.size() > 128) throw std::runtime_error("fixture exceeds 128 entries");
    }
    return view;
}
// Snapshot manifest: D deleted path, N new directory, B base reference, F private blob, L symlink.
static View load(const fs::path& base, const fs::path& snapshot) {
    auto baseView = scan(base); View view = baseView;
    if (snapshot.empty()) return view;
    std::ifstream manifest(snapshot / L"manifest.tsv", std::ios::binary); std::string raw;
    if (!manifest) throw std::runtime_error("snapshot manifest missing");
    while (std::getline(manifest, raw)) {
        std::vector<std::wstring> field; size_t start = 0; auto line = wide(raw);
        for (size_t i = 0; i <= line.size(); ++i) if (i == line.size() || line[i] == L'\t') { field.push_back(line.substr(start, i - start)); start = i + 1; }
        if (field.size() < 2 || field[0].size() != 1 || field[1].empty()) throw std::runtime_error("invalid snapshot record");
        auto& path = field[1];
        if (field[0] == L"D") {
            for (auto it = view.begin(); it != view.end();) it = same(it->first, path) || under(it->first, path) ? view.erase(it) : std::next(it);
        } else if (field[0] == L"N") { Entry entry; entry.kind = Directory; entry.info = now(FILE_ATTRIBUTE_DIRECTORY); view[path] = entry; }
        else if (field.size() == 3 && field[0] == L"B") view[path] = baseView.at(field[2]);
        else if (field.size() == 3 && field[0] == L"F") {
            Entry entry; entry.source = (snapshot / L"blobs" / field[2]).wstring(); entry.size = static_cast<INT64>(fs::file_size(entry.source));
            entry.info = now(FILE_ATTRIBUTE_NORMAL); view[path] = entry;
        } else if (field.size() == 3 && field[0] == L"L") { Entry entry; entry.kind = Link; entry.target = field[2]; view[path] = entry; }
        else throw std::runtime_error("unknown snapshot record");
        if (view.size() > 128) throw std::runtime_error("view exceeds 128 entries");
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
        note("fetch-data key=%s flags=%lu required=%lld+%lld", utf8(key).c_str(), static_cast<unsigned long>(parameters->FetchData.Flags),
            parameters->FetchData.RequiredFileOffset.QuadPart, parameters->FetchData.RequiredLength.QuadPart);
        std::vector<char> data; NTSTATUS status = STATUS_SUCCESS;
        try {
            if (found == self->view.end() || found->second.kind != File) throw std::runtime_error("no file entry");
            if (same(leaf(key), L"slow.bin") && fs::exists(self->slow)) { note("fetch-slow-wait key=%s", utf8(key).c_str()); Sleep(45000); }
            data = bytes(found->second.source);
            if (static_cast<INT64>(data.size()) != found->second.size) throw std::runtime_error("immutable source size changed");
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
        root = owned; slow = root.wstring() + L".slow"; view = load(base, snapshot); self = this;
        auto marker = root.wstring() + L".registered";
        if (!fs::exists(marker)) {
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
            std::ofstream(fs::path(marker)) << "registered";
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
struct Local { Kind kind; std::wstring base, blob, target; CF_PLACEHOLDER_STATE state; };
static std::wstring placeholderIdentity(const fs::path& path) {
    HANDLE handle = CreateFileW(path.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    if (handle == INVALID_HANDLE_VALUE) throw std::runtime_error("open placeholder");
    std::vector<BYTE> buffer(sizeof(CF_PLACEHOLDER_BASIC_INFO) + 4096); DWORD returned = 0;
    HRESULT hr = CfGetPlaceholderInfo(handle, CF_PLACEHOLDER_INFO_BASIC, buffer.data(), static_cast<DWORD>(buffer.size()), &returned);
    CloseHandle(handle); require(hr, "CfGetPlaceholderInfo");
    auto info = reinterpret_cast<CF_PLACEHOLDER_BASIC_INFO*>(buffer.data());
    return std::wstring(reinterpret_cast<wchar_t*>(info->FileIdentity), info->FileIdentityLength / sizeof(wchar_t));
}
template <class Visit> static void walk(const fs::path& root, const std::wstring& relative, Visit visit) {
    WIN32_FIND_DATAW data{}; auto pattern = (relative.empty() ? root : root / relative) / L"*";
    HANDLE find = FindFirstFileExW(pattern.c_str(), FindExInfoBasic, &data, FindExSearchNameMatch, nullptr, 0);
    if (find == INVALID_HANDLE_VALUE) throw std::runtime_error("enumerate local directory");
    do {
        std::wstring name = data.cFileName;
        if (name == L"." || name == L"..") continue;
        auto path = relative.empty() ? name : relative + L"\\" + name;
        if (visit(path, data)) walk(root, path, visit);
    } while (FindNextFileW(find, &data));
    FindClose(find);
}
// Quiescent capture: the provider is stopped and partial directories are never enumerated.
static int capture(const fs::path& base, const fs::path& root, const fs::path& out) {
    expose();
    auto baseView = scan(base); std::map<std::wstring, Local, NoCase> local;
    fs::create_directories(out / L"blobs");
    walk(root, L"", [&](const std::wstring& path, const WIN32_FIND_DATAW& data) {
        auto full = root / path; auto state = CfGetPlaceholderStateFromFindData(&data);
        bool directory = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY;
        if ((data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) && data.dwReserved0 == IO_REPARSE_TAG_SYMLINK) {
            local[path] = { Link, L"", L"", fs::read_symlink(full).wstring(), state }; return false;
        }
        if (state != CF_PLACEHOLDER_STATE_INVALID && (state & CF_PLACEHOLDER_STATE_PLACEHOLDER)) {
            auto id = placeholderIdentity(full);
            if (directory && (state & CF_PLACEHOLDER_STATE_PARTIAL)) {
                for (const auto& [name, entry] : baseView)
                    if (same(name, id) || under(name, id)) local[path + name.substr(id.size())] = { entry.kind, name, L"", entry.target, state };
                return false;
            }
            if (directory) { local[path] = { Directory, id, L"", L"", state }; return true; }
            if (state & CF_PLACEHOLDER_STATE_IN_SYNC) { local[path] = { File, id, L"", L"", state }; return false; }
        } else if (directory) { local[path] = { Directory, L"", L"", L"", state }; return true; }
        auto content = bytes(full); auto digest = sha256(content);
        std::ofstream(out / L"blobs" / wide(digest), std::ios::binary).write(content.data(), static_cast<std::streamsize>(content.size()));
        local[path] = { File, L"", wide(digest), L"", state }; return false;
    });
    std::ofstream manifest(out / L"manifest.tsv", std::ios::binary); std::set<std::wstring, NoCase> deleted;
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
        if (argc == 5 && verb == L"capture") return capture(argv[2], argv[3], argv[4]);
        if (argc == 3 && verb == L"audit") return audit(argv[2]);
        if (argc == 3 && verb == L"unregister") { HRESULT hr = CfUnregisterSyncRoot(argv[2]); std::printf("{\"unregister\":\"%08lx\"}\n", static_cast<unsigned long>(hr)); return FAILED(hr); }
        if (argc == 4 && verb == L"move") return result("move", MoveFileExW(argv[2], argv[3], 0));
        if (argc == 6 && verb == L"hold") return hold(argv[2], argv[3], argv[4], argv[5]);
        if (argc == 3 && verb == L"flush-directory") return flushDirectory(argv[2]);
        return 2;
    } catch (const std::exception& error) { std::fprintf(stderr, "sparse proof error: %s\n", error.what()); return 2; }
}
