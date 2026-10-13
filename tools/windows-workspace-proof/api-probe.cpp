// SDK compilation and system-DLL export inspection only. No root is projected.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <projectedfslib.h>
#include <cstdio>
#include <cwchar>

int main() {
    std::printf("SDK PRJ_CALLBACKS=%zu PRJ_PLACEHOLDER_INFO=%zu\n",
        sizeof(PRJ_CALLBACKS), sizeof(PRJ_PLACEHOLDER_INFO));
    HMODULE library = LoadLibraryExW(L"ProjectedFSLib.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!library) {
        DWORD error = GetLastError();
        wchar_t message[1024]{};
        FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            nullptr, error, 0, message, 1024, nullptr);
        std::printf("LoadLibraryExW(ProjectedFSLib.dll,System32) error=%lu\n", error);
        std::wprintf(L"%ls\n", message);
        return 3;
    }
    bool available = true;
    const char* names[] = {"PrjMarkDirectoryAsPlaceholder", "PrjStartVirtualizing",
        "PrjStopVirtualizing", "PrjWritePlaceholderInfo", "PrjWriteFileData",
        "PrjGetOnDiskFileState", "PrjFileNameCompare", "PrjFileNameMatch",
        "PrjFillDirEntryBuffer", "PrjAllocateAlignedBuffer", "PrjFreeAlignedBuffer",
        "PrjWritePlaceholderInfo2", "PrjFillDirEntryBuffer2"};
    for (const auto name : names) {
        bool found = GetProcAddress(library, name) != nullptr;
        std::printf("export %s=%s\n", name, found ? "present" : "missing");
        available = available && found;
    }
    FreeLibrary(library);
    return available ? 0 : 3;
}
