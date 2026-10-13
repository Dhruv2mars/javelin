// Bounded fixture provider, audit and Windows job wrapper. No product integration.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <projectedfslib.h>
#include <objbase.h>
#include <filesystem>
#include <fstream>
#include <map>
#include <set>
#include <vector>
#include <mutex>
#include <algorithm>
#include <cstdio>
#include <cwchar>
#include <cstring>
#include <stdexcept>

namespace fs = std::filesystem;
struct Names { bool operator()(const std::wstring& a, const std::wstring& b) const { return PrjFileNameCompare(a.c_str(), b.c_str()) < 0; } };
struct Entry { std::wstring name, source, target; PRJ_FILE_BASIC_INFO info{}; };
struct Session { std::vector<const Entry*> entries; size_t next = 0; std::wstring search; bool initialized = false; };
struct GuidLess { bool operator()(const GUID& a, const GUID& b) const { return std::memcmp(&a, &b, sizeof(GUID)) < 0; } };

static std::string utf8(const std::wstring& text) {
    int count = WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0, nullptr, nullptr);
    std::string out(static_cast<size_t>(count), '\0');
    WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), out.data(), count, nullptr, nullptr);
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
static void require(HRESULT hr, const char* operation) {
    if (FAILED(hr)) { std::printf("%s HRESULT=0x%08lx\n", operation, static_cast<unsigned long>(hr)); throw std::runtime_error(operation); }
}
static std::map<std::wstring, Entry, Names> scan(const fs::path& base) {
    std::map<std::wstring, Entry, Names> entries;
    for (const auto& file : fs::recursive_directory_iterator(base)) {
        Entry entry;
        entry.name = file.path().lexically_relative(base).wstring();
        entry.source = file.path().wstring();
        bool symlink = file.is_symlink();
        entry.info.IsDirectory = !symlink && file.is_directory();
        entry.info.FileAttributes = entry.info.IsDirectory ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL;
        if (symlink) entry.target = fs::read_symlink(file.path()).wstring();
        else if (!entry.info.IsDirectory) {
            auto size = file.file_size();
            if (size > 256 * 1024) throw std::runtime_error("fixture file exceeds 256 KiB bound");
            entry.info.FileSize = static_cast<INT64>(size);
        }
        WIN32_FILE_ATTRIBUTE_DATA data{};
        if (!GetFileAttributesExW(entry.source.c_str(), GetFileExInfoStandard, &data)) throw std::runtime_error("base metadata");
        std::memcpy(&entry.info.CreationTime, &data.ftCreationTime, sizeof(FILETIME));
        std::memcpy(&entry.info.LastAccessTime, &data.ftLastAccessTime, sizeof(FILETIME));
        std::memcpy(&entry.info.LastWriteTime, &data.ftLastWriteTime, sizeof(FILETIME));
        entry.info.ChangeTime = entry.info.LastWriteTime;
        entries.emplace(entry.name, entry);
        if (entries.size() > 128) throw std::runtime_error("fixture exceeds 128 entries");
    }
    return entries;
}

struct Provider {
    std::vector<Entry> entries;
    std::map<GUID, Session, GuidLess> sessions;
    std::mutex mutex;
    PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT context{};
    const Entry* find(PCWSTR path) const {
        auto item = std::lower_bound(entries.begin(), entries.end(), path,
            [](const Entry& a, PCWSTR b) { return PrjFileNameCompare(a.name.c_str(), b) < 0; });
        return item != entries.end() && PrjFileNameCompare(item->name.c_str(), path) == 0 ? &*item : nullptr;
    }
    static Provider& self(const PRJ_CALLBACK_DATA* data) { return *static_cast<Provider*>(data->InstanceContext); }
    static HRESULT CALLBACK start(const PRJ_CALLBACK_DATA* data, const GUID* id) {
        auto& p = self(data); std::lock_guard lock(p.mutex);
        Session session;
        for (const auto& item : p.entries)
            if (PrjFileNameCompare(fs::path(item.name).parent_path().c_str(), data->FilePathName) == 0) session.entries.push_back(&item);
        p.sessions[*id] = std::move(session);
        return S_OK;
    }
    static HRESULT CALLBACK end(const PRJ_CALLBACK_DATA* data, const GUID* id) {
        auto& p = self(data); std::lock_guard lock(p.mutex); p.sessions.erase(*id); return S_OK;
    }
    static HRESULT CALLBACK enumerate(const PRJ_CALLBACK_DATA* data, const GUID* id, PCWSTR search, PRJ_DIR_ENTRY_BUFFER_HANDLE buffer) {
        auto& p = self(data); std::lock_guard lock(p.mutex);
        auto found = p.sessions.find(*id); if (found == p.sessions.end()) return E_INVALIDARG;
        auto& session = found->second;
        if (!session.initialized || (data->Flags & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN)) {
            session.search = search ? search : L"*"; session.next = 0; session.initialized = true;
        }
        bool filled = false;
        while (session.next < session.entries.size()) {
            const auto& entry = *session.entries[session.next];
            auto name = fs::path(entry.name).filename().wstring();
            if (!PrjFileNameMatch(name.c_str(), session.search.c_str())) { ++session.next; continue; }
            PRJ_EXTENDED_INFO extended{}; extended.InfoType = PRJ_EXT_INFO_TYPE_SYMLINK; extended.Symlink.TargetName = entry.target.c_str();
            HRESULT hr = entry.target.empty() ? PrjFillDirEntryBuffer(name.c_str(), &entry.info, buffer)
                : PrjFillDirEntryBuffer2(buffer, name.c_str(), &entry.info, &extended);
            if (hr == HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)) return filled ? S_OK : hr;
            if (FAILED(hr)) return hr;
            filled = true; ++session.next;
            if (data->Flags & PRJ_CB_DATA_FLAG_ENUM_RETURN_SINGLE_ENTRY) break;
        }
        return S_OK;
    }
    static HRESULT CALLBACK placeholder(const PRJ_CALLBACK_DATA* data) {
        auto& p = self(data); const auto* entry = p.find(data->FilePathName);
        if (!entry) return HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND);
        PRJ_PLACEHOLDER_INFO info{}; info.FileBasicInfo = entry->info;
        UINT64 identity = static_cast<UINT64>(entry - p.entries.data()) + 1;
        std::memcpy(info.VersionInfo.ContentID, &identity, sizeof(identity));
        PRJ_EXTENDED_INFO extended{}; extended.InfoType = PRJ_EXT_INFO_TYPE_SYMLINK; extended.Symlink.TargetName = entry->target.c_str();
        auto hr = entry->target.empty() ? PrjWritePlaceholderInfo(data->NamespaceVirtualizationContext, data->FilePathName, &info, sizeof(info))
            : PrjWritePlaceholderInfo2(data->NamespaceVirtualizationContext, data->FilePathName, &info, sizeof(info), &extended);
        std::printf("placeholder %s hr=%08lx\n", utf8(entry->name).c_str(), static_cast<unsigned long>(hr)); std::fflush(stdout);
        return hr;
    }
    static HRESULT CALLBACK contents(const PRJ_CALLBACK_DATA* data, UINT64 offset, UINT32 length) {
        auto& p = self(data); UINT64 identity = 0;
        if (data->VersionInfo) std::memcpy(&identity, data->VersionInfo->ContentID, sizeof(identity));
        const auto* entry = identity > 0 && identity <= p.entries.size() ? &p.entries[static_cast<size_t>(identity - 1)] : p.find(data->FilePathName);
        if (!entry || entry->info.IsDirectory || !entry->target.empty()) return HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND);
        auto size = static_cast<UINT32>(entry->info.FileSize);
        if (offset > size || length > size - offset) return E_INVALIDARG;
        if (size == 0) return S_OK;
        // The whole bounded file starts at aligned offset zero and ends at EOF.
        void* buffer = PrjAllocateAlignedBuffer(data->NamespaceVirtualizationContext, size);
        if (!buffer) return E_OUTOFMEMORY;
        HANDLE file = CreateFileW(entry->source.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, 0, nullptr);
        DWORD count = 0;
        HRESULT hr = S_OK;
        if (file == INVALID_HANDLE_VALUE) hr = HRESULT_FROM_WIN32(GetLastError());
        else {
            if (!ReadFile(file, buffer, size, &count, nullptr) || count != size) hr = E_FAIL;
            CloseHandle(file);
        }
        if (SUCCEEDED(hr)) hr = PrjWriteFileData(data->NamespaceVirtualizationContext, &data->DataStreamId, buffer, 0, size);
        PrjFreeAlignedBuffer(buffer);
        std::printf("hydrate %s requested=%llu+%lu supplied=%lu hr=%08lx\n", utf8(entry->name).c_str(), offset,
            static_cast<unsigned long>(length), static_cast<unsigned long>(size), static_cast<unsigned long>(hr)); std::fflush(stdout);
        return hr;
    }
    static HRESULT CALLBACK notification(const PRJ_CALLBACK_DATA* data, BOOLEAN, PRJ_NOTIFICATION notification,
        PCWSTR destination, PRJ_NOTIFICATION_PARAMETERS*) {
        std::printf("notification %lu %s destination=%s\n", static_cast<unsigned long>(notification),
            utf8(data->FilePathName).c_str(), destination ? utf8(destination).c_str() : ""); std::fflush(stdout);
        return S_OK; // Observe; do not reject writes, rename or deletes as RegFS does.
    }
    int serve(const fs::path& base, const fs::path& root, const fs::path& stop, const fs::path& ready, const fs::path& snapshot) {
        const auto backing = scan(base);
        auto index = backing;
        if (!snapshot.empty()) {
            std::wifstream deleted(snapshot / L"deleted.txt"); std::wstring name;
            while (std::getline(deleted, name)) {
                for (auto it = index.begin(); it != index.end();) {
                    if (PrjFileNameCompare(it->first.c_str(), name.c_str()) == 0 || it->first.starts_with(name + L"\\")) it = index.erase(it);
                    else ++it;
                }
            }
            for (const auto& [name, entry] : scan(snapshot / L"changed")) index[name] = entry;
            std::wifstream aliases(snapshot / L"aliases.txt");
            while (std::getline(aliases, name)) {
                auto tab = name.find(L'\t');
                if (tab == std::wstring::npos) throw std::runtime_error("invalid bounded alias record");
                auto destination = name.substr(0, tab), source = name.substr(tab + 1);
                auto entry = backing.at(source); entry.name = destination; index[destination] = entry;
            }
        }
        for (auto& [name, entry] : index) { (void)name; entries.push_back(std::move(entry)); }
        fs::create_directories(root);
        auto marker = root.wstring() + L".identity";
        if (!fs::exists(marker)) {
            GUID id{}; require(CoCreateGuid(&id), "CoCreateGuid");
            require(PrjMarkDirectoryAsPlaceholder(root.c_str(), nullptr, nullptr, &id), "PrjMarkDirectoryAsPlaceholder");
            std::ofstream output(fs::path(marker), std::ios::binary); output.write(reinterpret_cast<char*>(&id), sizeof(id));
        }
        PRJ_CALLBACKS callbacks{};
        callbacks.StartDirectoryEnumerationCallback = start; callbacks.EndDirectoryEnumerationCallback = end;
        callbacks.GetDirectoryEnumerationCallback = enumerate; callbacks.GetPlaceholderInfoCallback = placeholder;
        callbacks.GetFileDataCallback = contents; callbacks.NotificationCallback = notification;
        PRJ_NOTIFICATION_MAPPING mapping{}; mapping.NotificationRoot = L"";
        mapping.NotificationBitMask = static_cast<PRJ_NOTIFY_TYPES>(PRJ_NOTIFY_FILE_OPENED | PRJ_NOTIFY_NEW_FILE_CREATED |
            PRJ_NOTIFY_FILE_OVERWRITTEN | PRJ_NOTIFY_PRE_DELETE | PRJ_NOTIFY_PRE_RENAME | PRJ_NOTIFY_FILE_RENAMED |
            PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED | PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED);
        PRJ_STARTVIRTUALIZING_OPTIONS options{}; options.NotificationMappings = &mapping; options.NotificationMappingsCount = 1;
        require(PrjStartVirtualizing(root.c_str(), &callbacks, this, &options, &context), "PrjStartVirtualizing");
        PRJ_VIRTUALIZATION_INSTANCE_INFO info{}; require(PrjGetVirtualizationInstanceInfo(context, &info), "PrjGetVirtualizationInstanceInfo");
        wchar_t idText[40]{}; StringFromGUID2(info.InstanceID, idText, 40);
        std::printf("started root=%s instance=%s alignment=%lu entries=%zu\n", utf8(root.wstring()).c_str(), utf8(idText).c_str(),
            static_cast<unsigned long>(info.WriteAlignment), entries.size()); std::fflush(stdout);
        std::ofstream(ready) << utf8(idText);
        ULONGLONG deadline = GetTickCount64() + 180000;
        while (!fs::exists(stop) && GetTickCount64() < deadline) Sleep(50);
        PrjStopVirtualizing(context); context = nullptr;
        std::printf("stopped root=%s\n", utf8(root.wstring()).c_str()); std::fflush(stdout);
        return fs::exists(stop) ? 0 : 124;
    }
    ~Provider() { if (context) PrjStopVirtualizing(context); }
};

static int audit(const fs::path& root, const fs::path& base) {
    // The caller has stopped the provider. A query cannot hydrate from backing data.
    std::set<std::wstring, Names> names;
    for (const auto& [name, entry] : scan(base)) { (void)entry; names.insert(name); }
    std::error_code error;
    fs::recursive_directory_iterator iterator(root, fs::directory_options::none, error), end;
    while (!error && iterator != end) { names.insert(iterator->path().lexically_relative(root).wstring()); iterator.increment(error); }
    std::printf("{\"walkError\":%d,\"entries\":[", error.value());
    bool first = true;
    for (const auto& name : names) {
        auto path = root / name; PRJ_FILE_STATE state{};
        HRESULT hr = PrjGetOnDiskFileState(path.c_str(), &state);
        DWORD attributes = GetFileAttributesW(path.c_str());
        INT64 bytes = 0, allocation = 0; DWORD attributeError = 0;
        HANDLE handle = CreateFileW(path.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, nullptr);
        if (handle != INVALID_HANDLE_VALUE) {
            FILE_STANDARD_INFO info{};
            if (GetFileInformationByHandleEx(handle, FileStandardInfo, &info, sizeof(info))) { bytes = info.EndOfFile.QuadPart; allocation = info.AllocationSize.QuadPart; }
            else attributeError = GetLastError();
            CloseHandle(handle);
        } else attributeError = GetLastError();
        if (!first) std::printf(","); first = false;
        std::printf("{\"path\":%s,\"state\":%u,\"hresult\":%ld,\"attributes\":%lu,\"logicalBytes\":%lld,\"allocatedBytes\":%lld,\"attributeError\":%lu}",
            json(name).c_str(), static_cast<unsigned>(state), static_cast<long>(hr), static_cast<unsigned long>(attributes), bytes, allocation,
            static_cast<unsigned long>(attributeError));
    }
    std::printf("]}\n"); return 0;
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
static int bounded(int argc, wchar_t** argv) {
    DWORD milliseconds = static_cast<DWORD>(std::stoul(argv[2]));
    if (milliseconds > 120000 || milliseconds < 100) return 2;
    std::wstring command;
    for (int i = 3; i < argc; ++i) { if (i > 3) command += L' '; command += quote(argv[i]); }
    HANDLE job = CreateJobObjectW(nullptr, nullptr);
    if (!job) return 2;
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{}; limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) { CloseHandle(job); return 2; }
    STARTUPINFOW startup{}; startup.cb = sizeof(startup);
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
    TerminateJobObject(job, 124); // Also stop remaining children after a normal parent exit.
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting{};
    ULONGLONG deadline = GetTickCount64() + 5000;
    do {
        QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), nullptr);
        if (accounting.ActiveProcesses == 0) break; Sleep(20);
    } while (GetTickCount64() < deadline);
    std::fprintf(stderr, "WINDOWS_JOB parent=%lu timedOut=%d exit=%lu activeAfterTermination=%lu\n", child.dwProcessId, timedOut,
        code, accounting.ActiveProcesses);
    CloseHandle(child.hProcess); CloseHandle(job);
    return accounting.ActiveProcesses == 0 ? static_cast<int>(code) : 125;
}

int wmain(int argc, wchar_t** argv) {
    try {
        wchar_t hosted[32]{}, actions[16]{};
        GetEnvironmentVariableW(L"RUNNER_ENVIRONMENT", hosted, 32); GetEnvironmentVariableW(L"GITHUB_ACTIONS", actions, 16);
        if (std::wcscmp(hosted, L"github-hosted") != 0 || std::wcscmp(actions, L"true") != 0) return 2;
        if (argc >= 4 && std::wcscmp(argv[1], L"bounded") == 0) return bounded(argc, argv);
        if (argc == 4 && std::wcscmp(argv[1], L"audit") == 0) return audit(argv[2], argv[3]);
        if ((argc == 6 || argc == 7) && std::wcscmp(argv[1], L"serve") == 0) {
            Provider provider; return provider.serve(argv[2], argv[3], argv[4], argv[5], argc == 7 ? fs::path(argv[6]) : fs::path());
        }
        return 2;
    } catch (const std::exception& error) { std::fprintf(stderr, "native proof error: %s\n", error.what()); return 2; }
}
