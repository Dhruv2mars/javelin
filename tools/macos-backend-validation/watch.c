#include <CoreServices/CoreServices.h>
#include <sys/event.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>

static unsigned events = 0;
static void callback(ConstFSEventStreamRef stream, void *info, size_t n,
                     void *paths, const FSEventStreamEventFlags flags[],
                     const FSEventStreamEventId ids[]) {
    (void)stream; (void)info;
    char **names = paths;
    for (size_t i = 0; i < n; i++) {
        printf("FSEvent path=%s flags=%u id=%llu\n", names[i], flags[i], ids[i]);
        events++;
    }
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    int fd = open(argv[1], O_EVTONLY), kq = kqueue();
    struct kevent change, event;
    EV_SET(&change, fd, EVFILT_VNODE, EV_ADD | EV_CLEAR,
           NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_RENAME, 0, NULL);
    int registered = fd >= 0 && kq >= 0 ? kevent(kq, &change, 1, NULL, 0, NULL) : -1;
    CFStringRef path = CFStringCreateWithCString(NULL, argv[1], kCFStringEncodingUTF8);
    CFArrayRef paths = CFArrayCreate(NULL, (const void **)&path, 1, &kCFTypeArrayCallBacks);
    FSEventStreamRef stream = FSEventStreamCreate(NULL, callback, NULL, paths,
        kFSEventStreamEventIdSinceNow, 0.1,
        kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer);
    Boolean started = false;
    if (stream) {
        FSEventStreamScheduleWithRunLoop(stream, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        started = FSEventStreamStart(stream);
    }
    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.3, false);
    printf("watcher-ready fsevents=%d kqueue=%d\n", started, registered);
    fflush(stdout);
    char target[4096];
    snprintf(target, sizeof(target), "%s/watch-created", argv[1]);
    int file = open(target, O_CREAT | O_WRONLY | O_TRUNC, 0600);
    int mutation = file >= 0 ? (int)write(file, "watch\n", 6) : -1;
    int synced = file >= 0 ? fsync(file) : -1;
    if (file >= 0) close(file);
    char nested[4096];
    snprintf(nested, sizeof(nested), "%s/nested", argv[1]);
    mkdir(nested, 0700);
    snprintf(nested, sizeof(nested), "%s/nested/child", argv[1]);
    int child = open(nested, O_CREAT | O_WRONLY, 0600);
    if (child >= 0) { write(child, "nested\n", 7); fsync(child); close(child); }
    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 3.0, false);
    struct timespec timeout = {0, 0};
    int count = registered == 0 ? kevent(kq, NULL, 0, &event, 1, &timeout) : -1;
    printf("{\"fseventsStarted\":%s,\"fseventsCount\":%u,\"kqueueRegistered\":%d,"
           "\"kqueueCount\":%d,\"kqueueFlags\":%u,\"mutationBytes\":%d,\"fsync\":%d}\n",
           started ? "true" : "false", events, registered, count,
           count > 0 ? event.fflags : 0, mutation, synced);
    if (stream) { if (started) FSEventStreamStop(stream); FSEventStreamInvalidate(stream); FSEventStreamRelease(stream); }
    CFRelease(paths); CFRelease(path);
    if (fd >= 0) close(fd);
    if (kq >= 0) close(kq);
    return mutation == 6 && synced == 0 ? 0 : 1;
}
