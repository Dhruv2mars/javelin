#import <Foundation/Foundation.h>
#import <FSKit/FSKit.h>
#include <stdatomic.h>
#include <stdbool.h>

static atomic_bool done;
static atomic_int result;

int main(void) {
    @autoreleasepool {
        [[FSClient sharedInstance] fetchInstalledExtensionsWithCompletionHandler:
         ^(NSArray<FSModuleIdentity *> *modules, NSError *error) {
            NSMutableArray *rows = [NSMutableArray array];
            for (FSModuleIdentity *module in modules) {
                [rows addObject:@{@"bundleIdentifier": module.bundleIdentifier,
                                  @"enabled": @(module.isEnabled),
                                  @"path": module.url.path}];
            }
            NSDictionary *output = @{@"modules": rows,
                @"error": error ? error.description : [NSNull null],
                @"pathResourceClass": @(NSClassFromString(@"FSPathURLResource") != nil),
                @"mountClientSelector": @([[FSClient sharedInstance] respondsToSelector:
                    NSSelectorFromString(@"mountSingleVolumeForResource:bundleID:options:completionHandler:")])};
            NSData *data = [NSJSONSerialization dataWithJSONObject:output options:0 error:nil];
            puts([[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding].UTF8String);
            atomic_store(&result, error ? 1 : 0);
            atomic_store(&done, true);
        }];
        NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:5];
        while (!atomic_load(&done) && deadline.timeIntervalSinceNow > 0) {
            [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
        }
        if (!atomic_load(&done)) { fprintf(stderr, "FSClient discovery timed out after 5 seconds\n"); return 2; }
        return atomic_load(&result);
    }
}
