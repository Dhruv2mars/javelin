#import <Foundation/Foundation.h>
#import <FSKit/FSKit.h>

int main(void) {
    @autoreleasepool {
        __block BOOL done = NO;
        __block int result = 0;
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
            result = error ? 1 : 0;
            done = YES;
        }];
        NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:5];
        while (!done && deadline.timeIntervalSinceNow > 0) {
            [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
        }
        if (!done) { fprintf(stderr, "FSClient discovery timed out after 5 seconds\n"); return 2; }
        return result;
    }
}
