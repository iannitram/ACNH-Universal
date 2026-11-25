// The iPhone and iPad first-start game download (issue #231; SPEC M18). The small .ipa holds the player and no game;
// when the game root has no analysis/main.bin, platform_mac.cpp calls nx_appgame_first_start (on the game's thread)
// before the game boots. This file shows the screens (Dropbox sign-in, "Download the game (about N GB)?", progress)
// and moves the bytes in a background URLSession; the plan, the hash checks and the install are Rust
// (crates/runtime/src/appgame.rs through acnh_appgame_*, which build on acnh_core::appgame), and the Dropbox
// sign-in and token are Rust too (appgame_ios.rs).
//
// The game goes to Library/Application Support/game/ (read-only once complete, excluded from iCloud backup);
// game.partial/ holds the files so far. The files are fetched in chunks of kChunk bytes, one background task at a
// time (a Range request each: Dropbox's download endpoint is a POST, which gives NSURLSession no resume data), and
// each finished chunk is appended to its file. So what is on disk is always a whole number of chunks, and a restart
// (force-quit, the system ending the app, no network for a while) resumes from the file's length: the Rust plan
// reads the folder and says where each file stands. The session sends launch events: when a chunk finishes with the
// app suspended or closed, iOS relaunches it in the background and the delegate starts the next one
// (application:handleEventsForBackgroundURLSession:completionHandler:, added to SDL's app delegate at load).
//
// Not verified on a device: everything here is compile-checked only (tools/check_ios_compile.sh); the owner's iPad
// is the test (issue #231, "On the owner's iPad").
#if !__has_feature(objc_arc)
#error "appgame_ios.mm needs -fobjc-arc"
#endif

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>
#import <objc/runtime.h>

#include <cstdint>
#include <cstdlib>
#include <string>

// crates/runtime/src/appgame_ios.rs
extern "C" {
void acnh_appgame_free(char*);
char* acnh_appgame_plan(const char* root, const uint8_t* manifest, size_t manifest_len, const char* abi,
                        const char* builds, uint64_t free_bytes);
char* acnh_appgame_finish(const char* root);
bool acnh_appgame_installed_ok(const char* root, const char* abi, const char* builds);
char* acnh_appgame_api_arg(const char* path);
bool acnh_appgame_signed_in(void);
char* acnh_appgame_sign_in_start(void);
char* acnh_appgame_sign_in_finish(const char* code);
char* acnh_appgame_token(void);
void acnh_appgame_sign_out(void);
void nx_relay_log(const char* line);  // platform_mac.cpp: one line into the player's log
}

namespace {

// Big, so an 11 GB game is a dozen relaunches of the app while locked, not ~90 (each task started from the background is
// scheduled at the system's discretion and rate limited).
constexpr int64_t kChunk = 1024ll << 20;  // bytes per background task
constexpr int kRetries = 6;              // a chunk or the plan failing this many times in a row stops the download
NSString* const kSessionId = @"io.github.acnhrecomp.game-download";
NSString* const kPending = @"game.manifest.pending.json";  // appgame.rs: PENDING
NSString* const kRemoteHost = @"https://content.dropboxapi.com/2/files/download";

#ifdef NX_APPGAME_ABI
const char* const kAbi = NX_APPGAME_ABI;
#else
const char* const kAbi = "";
#endif
// The game build(s) this app was compiled from, comma separated (tools/build_ios.sh passes NX_APPGAME_BUILDS from the
// recompiled game's game.ini): the game's code is linked into the player, so it can run only that build's files.
// Empty (no game.ini beside the code) accepts any.
#ifdef NX_APPGAME_BUILDS
const char* const kBuilds = NX_APPGAME_BUILDS;
#else
const char* const kBuilds = "";
#endif

void note(NSString* line) { nx_relay_log([[@"appgame: " stringByAppendingString:line] UTF8String]); }

// ---- Rust's JSON answers ---------------------------------------------------------------------------------------

NSDictionary* take(char* raw) {
    if (!raw) return @{@"ok" : @NO, @"error" : @"no answer"};
    NSData* data = [NSData dataWithBytes:raw length:strlen(raw)];
    acnh_appgame_free(raw);
    id parsed = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    return [parsed isKindOfClass:[NSDictionary class]] ? parsed : @{@"ok" : @NO, @"error" : @"unreadable answer"};
}

bool ok(NSDictionary* d) { return [d[@"ok"] boolValue]; }
NSString* why(NSDictionary* d) { return d[@"error"] ?: @"Something went wrong."; }

// ---- where things are ------------------------------------------------------------------------------------------

NSString* support_root() {
    static NSString* root;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        NSArray* dirs = NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory, NSUserDomainMask, YES);
        root = dirs.count ? dirs[0] : [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support"];
        [[NSFileManager defaultManager] createDirectoryAtPath:root withIntermediateDirectories:YES attributes:nil error:nil];
    });
    return root;
}

NSString* game_path() { return [support_root() stringByAppendingPathComponent:@"game"]; }
NSString* partial_path() { return [support_root() stringByAppendingPathComponent:@"game.partial"]; }
NSString* pending_path() { return [support_root() stringByAppendingPathComponent:kPending]; }

void exclude_from_backup(NSString* path) {
    NSURL* url = [NSURL fileURLWithPath:path];
    NSError* e = nil;
    if (![url setResourceValue:@YES forKey:NSURLIsExcludedFromBackupKey error:&e] && e)
        note([NSString stringWithFormat:@"could not exclude %@ from backup: %@", path.lastPathComponent, e.localizedDescription]);
}

uint64_t free_bytes() {
    NSURL* url = [NSURL fileURLWithPath:support_root()];
    NSDictionary* v = [url resourceValuesForKeys:@[ NSURLVolumeAvailableCapacityForImportantUsageKey ] error:nil];
    NSNumber* n = v[NSURLVolumeAvailableCapacityForImportantUsageKey];
    return n ? (uint64_t)n.longLongValue : 0;
}

NSString* gb(double bytes) { return [NSString stringWithFormat:@"%.1f GB", bytes / 1e9]; }

// ---- the screens -----------------------------------------------------------------------------------------------

UIWindowScene* active_scene() API_AVAILABLE(ios(13.0)) {
    UIWindowScene* any = nil;
    for (UIScene* s in UIApplication.sharedApplication.connectedScenes) {
        if (![s isKindOfClass:[UIWindowScene class]]) continue;
        if (s.activationState == UISceneActivationStateForegroundActive) return (UIWindowScene*)s;
        if (!any) any = (UIWindowScene*)s;
    }
    return any;
}

// One window of our own on the app's scene, over the empty screen the player has before its game window exists.
}  // namespace (Objective-C declarations are global)

@interface NXProgressController : UIViewController
@property(nonatomic, strong) UILabel* title_label;
@property(nonatomic, strong) UILabel* status_label;
@property(nonatomic, strong) UIProgressView* bar;
@end

@implementation NXProgressController
- (void)viewDidLoad {
    [super viewDidLoad];
    self.view.backgroundColor = UIColor.blackColor;
    self.title_label = [UILabel new];
    self.title_label.text = @"Downloading the game";
    self.title_label.textColor = UIColor.whiteColor;
    self.title_label.font = [UIFont boldSystemFontOfSize:24];
    self.status_label = [UILabel new];
    self.status_label.textColor = UIColor.lightGrayColor;
    self.status_label.numberOfLines = 0;
    self.status_label.textAlignment = NSTextAlignmentCenter;
    self.bar = [[UIProgressView alloc] initWithProgressViewStyle:UIProgressViewStyleDefault];
    UIStackView* stack = [[UIStackView alloc] initWithArrangedSubviews:@[ self.title_label, self.bar, self.status_label ]];
    stack.axis = UILayoutConstraintAxisVertical;
    stack.spacing = 16;
    stack.alignment = UIStackViewAlignmentFill;
    stack.translatesAutoresizingMaskIntoConstraints = NO;
    [self.view addSubview:stack];
    [NSLayoutConstraint activateConstraints:@[
        [stack.centerYAnchor constraintEqualToAnchor:self.view.centerYAnchor],
        [stack.centerXAnchor constraintEqualToAnchor:self.view.centerXAnchor],
        [stack.widthAnchor constraintEqualToAnchor:self.view.widthAnchor multiplier:0.6],
    ]];
}
- (UIInterfaceOrientationMask)supportedInterfaceOrientations { return UIInterfaceOrientationMaskLandscape; }
@end

namespace {

UIWindow* g_window;
NXProgressController* g_progress;

// A window on the main thread, for an alert or the progress screen. Created when there is none. nil when there is
// nowhere to show one yet (a background launch has no scene, and a window made then is never shown): callers try again.
UIViewController* host_controller() {
    UIWindowScene* scene = active_scene();
    if (g_window && !g_window.windowScene && scene) {  // made before the scene connected: it would never be shown
        g_window.hidden = YES;
        g_window = nil;
        g_progress = nil;
    }
    if (!g_window) {
        if (!scene && UIApplication.sharedApplication.applicationState != UIApplicationStateActive) return nil;
        g_window = scene ? [[UIWindow alloc] initWithWindowScene:scene] : [[UIWindow alloc] initWithFrame:UIScreen.mainScreen.bounds];
        g_window.rootViewController = [UIViewController new];
        g_window.rootViewController.view.backgroundColor = UIColor.blackColor;
        g_window.windowLevel = UIWindowLevelAlert + 1;
        [g_window makeKeyAndVisible];
    }
    UIViewController* top = g_window.rootViewController;
    while (top.presentedViewController) top = top.presentedViewController;
    return top;
}

void close_window() {
    if (!g_window) return;
    g_window.hidden = YES;
    g_window = nil;
    g_progress = nil;
}

// Present `a` as soon as there is a window to show it in.
void present_alert(UIAlertController* a) {
    UIViewController* host = host_controller();
    if (!host) {
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC / 2), dispatch_get_main_queue(), ^{ present_alert(a); });
        return;
    }
    [host presentViewController:a animated:NO completion:nil];
}

// Block the game's thread until the app is in the foreground (a background launch has no screen to ask on, and the
// game must not boot, and so must not touch the GPU, there).
void wait_until_active() {
    dispatch_semaphore_t go = dispatch_semaphore_create(0);
    dispatch_async(dispatch_get_main_queue(), ^{
        if (UIApplication.sharedApplication.applicationState == UIApplicationStateActive) {
            dispatch_semaphore_signal(go);
            return;
        }
        __block id observer = nil;
        observer = [NSNotificationCenter.defaultCenter addObserverForName:UIApplicationDidBecomeActiveNotification object:nil
                                                                     queue:NSOperationQueue.mainQueue
                                                                usingBlock:^(NSNotification*) {
            [NSNotificationCenter.defaultCenter removeObserver:observer];
            dispatch_semaphore_signal(go);
        }];
    });
    dispatch_semaphore_wait(go, DISPATCH_TIME_FOREVER);
}

// An alert, answered from the game's thread: returns the tapped button's index (cancel style: the last one is "no").
// With text_field non-null, the field's text comes back in *text_field.
int ask(NSString* title, NSString* message, NSArray<NSString*>* buttons, NSString* __strong* text_field = nullptr,
        NSString* placeholder = nil) {
    dispatch_semaphore_t answered = dispatch_semaphore_create(0);
    __block int tapped = 0;
    __block NSString* typed = @"";
    dispatch_async(dispatch_get_main_queue(), ^{
        UIAlertController* a = [UIAlertController alertControllerWithTitle:title message:message preferredStyle:UIAlertControllerStyleAlert];
        if (text_field) [a addTextFieldWithConfigurationHandler:^(UITextField* f) {
            f.placeholder = placeholder;
            f.autocorrectionType = UITextAutocorrectionTypeNo;
            f.autocapitalizationType = UITextAutocapitalizationTypeNone;
        }];
        for (NSUInteger i = 0; i < buttons.count; i++) {
            UIAlertActionStyle style = (i + 1 == buttons.count && buttons.count > 1) ? UIAlertActionStyleCancel : UIAlertActionStyleDefault;
            [a addAction:[UIAlertAction actionWithTitle:buttons[i] style:style handler:^(UIAlertAction*) {
                tapped = (int)i;
                if (text_field) typed = a.textFields.firstObject.text ?: @"";
                dispatch_semaphore_signal(answered);
            }]];
        }
        present_alert(a);
    });
    dispatch_semaphore_wait(answered, DISPATCH_TIME_FOREVER);
    if (text_field) *text_field = typed;
    return tapped;
}

void show_progress(NSString* status, double fraction) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!g_progress) {
            UIViewController* top = host_controller();
            if (!top) return;  // no screen yet (a background launch): the next update shows it
            g_progress = [NXProgressController new];
            g_progress.modalPresentationStyle = UIModalPresentationFullScreen;
            [top presentViewController:g_progress animated:NO completion:nil];
        }
        (void)g_progress.view;  // loads the views
        g_progress.status_label.text = status;
        g_progress.bar.progress = (float)fraction;
    });
}

// ---- the download ----------------------------------------------------------------------------------------------

// One chunk task's description: "<path>\n<offset>\n<length>\n<file size>". Kept on the task, so a relaunched app knows
// what a finished task was for.
NSString* describe(NSString* path, int64_t offset, int64_t length, int64_t size) {
    return [NSString stringWithFormat:@"%@\n%lld\n%lld\n%lld", path, offset, length, size];
}

bool parse_description(NSString* d, NSString** path, int64_t* offset, int64_t* length, int64_t* size) {
    NSArray* p = [d componentsSeparatedByString:@"\n"];
    if (p.count != 4) return false;
    *path = p[0];
    *offset = [p[1] longLongValue];
    *length = [p[2] longLongValue];
    *size = [p[3] longLongValue];
    return true;
}

}  // namespace

// The system's completion handler for the session's events: kept before the session exists (it starts delivering at once).
static void (^g_background_done)(void);
static id background_lock() {
    static id lock;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ lock = [NSObject new]; });
    return lock;
}

@interface NXGameDownloader : NSObject <NSURLSessionDownloadDelegate>
@property(nonatomic, strong) NSURLSession* session;
@property(nonatomic, strong) dispatch_queue_t queue;  // all state below, and every step
@property(nonatomic, strong) dispatch_semaphore_t finished;
@property(nonatomic, copy) NSString* failure;  // set before `finished` is signalled; nil = success
@property(nonatomic, assign) int64_t total, base;  // all bytes; bytes on disk when the current chunk began
@property(nonatomic, assign) int64_t inflight;     // bytes of the running chunk so far
@property(nonatomic, assign) int failures;
@property(nonatomic, strong) NSMutableDictionary<NSString*, NSNumber*>* fetched;  // path -> full fetches finished (last chunk landed)
// One chain at a time, all on `queue`: `active` while a task is in flight or a step/retry is scheduled, `current` the
// identifier of the task in flight, `halted` after the chain stopped (failed or installed) until -begin.
@property(nonatomic, assign) bool active, halted;
@property(nonatomic, strong) NSNumber* current;
@property(nonatomic, copy) NSString* chunk_error;  // what went wrong with the chunk whose task just finished
@property(nonatomic, strong) NSDate* begin_after;  // the next task waits until then (a retry): the system runs it, even with the app suspended
+ (instancetype)shared;
- (dispatch_semaphore_t)begin;
- (NSString*)raw_api_arg:(NSString*)path;
- (void)step;
- (void)stop:(NSString*)failure;
- (void)retry:(NSString*)text;
- (void)retry_later:(NSString*)text;
@end

@implementation NXGameDownloader

+ (instancetype)shared {
    static NXGameDownloader* d;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        d = [NXGameDownloader new];
        d.queue = dispatch_queue_create("io.github.acnhrecomp.game-download", DISPATCH_QUEUE_SERIAL);
        d.fetched = [NSMutableDictionary new];
        NSURLSessionConfiguration* c = [NSURLSessionConfiguration backgroundSessionConfigurationWithIdentifier:kSessionId];
        c.sessionSendsLaunchEvents = YES;
        c.discretionary = NO;
        c.allowsCellularAccess = NO;  // 11 GB: Wi-Fi only
        c.timeoutIntervalForResource = 7 * 24 * 3600.0;
        NSOperationQueue* ops = [NSOperationQueue new];
        ops.maxConcurrentOperationCount = 1;
        d.session = [NSURLSession sessionWithConfiguration:c delegate:d delegateQueue:ops];
    });
    return d;
}

// Start (or take up) the chain, and return the semaphore signalled when it ends (`failure` says how). With a chain
// already going (the system relaunched the app for a finished chunk, or a retry is waiting) or a task still there from
// before a relaunch, nothing new starts: that chain goes on and is the only one.
- (dispatch_semaphore_t)begin {
    __block dispatch_semaphore_t done;
    dispatch_sync(self.queue, ^{
        self.halted = false;
        self.failure = nil;
        self.failures = 0;
        [self.fetched removeAllObjects];  // Try again is a fresh attempt at every file
        if (!self.active || !self.finished) self.finished = dispatch_semaphore_create(0);
        done = self.finished;
    });
    [self.session getAllTasksWithCompletionHandler:^(NSArray<__kindof NSURLSessionTask*>* tasks) {
        dispatch_async(self.queue, ^{
            if (self.active) { note(@"a download is already going on: waiting for it"); return; }
            NSURLSessionTask* adopt = nil;
            for (NSURLSessionTask* t in tasks)
                if (t.state == NSURLSessionTaskStateRunning || t.state == NSURLSessionTaskStateSuspended) { adopt = t; break; }
            if (!adopt) adopt = tasks.firstObject;  // finished while the app was gone: its events are still to come
            self.active = true;
            if (adopt) {
                note(@"a task from before is still there: waiting for it");
                self.current = @(adopt.taskIdentifier);
                if (adopt.state == NSURLSessionTaskStateSuspended) [adopt resume];
                return;
            }
            [self step];
        });
    }];
    return done;
}

// The chain ends: `failure` nil is success (installed). On self.queue.
- (void)stop:(NSString*)failure {
    self.failure = failure;
    self.active = false;
    self.halted = true;
    self.current = nil;
    if (!self.finished) self.finished = dispatch_semaphore_create(0);
    dispatch_semaphore_signal(self.finished);
}

// Whether `t` belongs to the one chain: the task in flight, or, with none yet, the first to report (a task finished
// while the app was gone) which then is. A late task of an older chain is not. On self.queue.
- (bool)claim:(NSURLSessionTask*)t {
    if (self.halted) return false;
    if (self.current) return self.current.unsignedIntegerValue == t.taskIdentifier;
    if (self.active) return false;
    self.active = true;
    self.current = @(t.taskIdentifier);
    if (!self.finished) self.finished = dispatch_semaphore_create(0);
    return true;
}

// Plan from the disk, then start the next chunk, or install when nothing is left. On self.queue.
- (void)step {
    NSData* manifest = [NSData dataWithContentsOfFile:pending_path()];
    if (!manifest) { [self stop:@"The file list is missing: start the download again."]; return; }
    NSDictionary* plan = take(acnh_appgame_plan(support_root().UTF8String, (const uint8_t*)manifest.bytes, manifest.length,
                                                kAbi, kBuilds, free_bytes()));
    if (!ok(plan)) { [self stop:why(plan)]; return; }
    self.total = [plan[@"total"] longLongValue];
    int64_t remaining = [plan[@"remaining"] longLongValue];
    self.base = self.total - remaining;
    self.inflight = 0;
    NSArray* fetch = plan[@"fetch"];
    if ([plan[@"installed"] boolValue]) { [self stop:nil]; return; }
    if (fetch.count == 0) {
        NSDictionary* done = take(acnh_appgame_finish(support_root().UTF8String));
        if (!ok(done)) { [self stop:why(done)]; return; }
        exclude_from_backup(game_path());
        note(@"the game is installed");
        [self stop:nil];
        return;
    }
    NSDictionary* next = fetch[0];
    NSString* path = next[@"path"];
    int64_t size = [next[@"size"] longLongValue], offset = [next[@"offset"] longLongValue];
    // A file whose last chunk landed twice and that starts from 0 again failed its hash check each time (Rust deleted
    // it): enough. A fetch that stopped on the way (no network) is not counted: only a last chunk landing is.
    if (offset == 0 && self.fetched[path].unsignedIntegerValue >= 2) {
        [self stop:[NSString stringWithFormat:@"%@ did not download correctly, even fetched again. Try again later.", path]];
        return;
    }
    int64_t length = MIN(kChunk, size - offset);
    NSDictionary* tok = take(acnh_appgame_token());
    if (!ok(tok)) {
        if ([tok[@"reauth"] boolValue]) { [self stop:@"Dropbox needs you to sign in again."]; return; }
        [self retry_later:why(tok)];
        return;
    }
    NSMutableURLRequest* r = [NSMutableURLRequest requestWithURL:[NSURL URLWithString:kRemoteHost]];
    r.HTTPMethod = @"POST";
    NSString* argText = [self raw_api_arg:path];
    [r setValue:[@"Bearer " stringByAppendingString:tok[@"token"]] forHTTPHeaderField:@"Authorization"];
    [r setValue:argText forHTTPHeaderField:@"Dropbox-API-Arg"];
    [r setValue:[NSString stringWithFormat:@"bytes=%lld-%lld", offset, offset + length - 1] forHTTPHeaderField:@"Range"];
    NSURLSessionDownloadTask* t = [self.session downloadTaskWithRequest:r];
    t.taskDescription = describe(path, offset, length, size);
    self.active = true;
    self.current = @(t.taskIdentifier);
    self.chunk_error = nil;
    if (self.begin_after) { t.earliestBeginDate = self.begin_after; self.begin_after = nil; }
    [t resume];
    show_progress([self status_text], self.total ? (double)self.base / self.total : 0);
}

- (NSString*)raw_api_arg:(NSString*)path {
    char* raw = acnh_appgame_api_arg(path.UTF8String);  // a JSON string, quoted
    if (!raw) return @"";
    NSData* data = [NSData dataWithBytes:raw length:strlen(raw)];
    acnh_appgame_free(raw);
    id s = [NSJSONSerialization JSONObjectWithData:data options:NSJSONReadingFragmentsAllowed error:nil];
    return [s isKindOfClass:[NSString class]] ? s : @"";
}

- (NSString*)status_text {
    return [NSString stringWithFormat:@"%@ of %@", gb((double)(self.base + self.inflight)), gb((double)self.total)];
}

// A transient failure of a chunk: try again after a pause, a few times. The next task carries the pause as its
// earliestBeginDate, so the system starts it even when the app is suspended (a timer there would never fire). On self.queue.
- (void)retry:(NSString*)text {
    self.failures++;
    note([NSString stringWithFormat:@"failed (%d): %@", self.failures, text]);
    if (self.failures >= kRetries) { [self stop:text]; return; }
    self.begin_after = [NSDate dateWithTimeIntervalSinceNow:MIN(60, 5 << self.failures)];
    self.current = nil;
    [self step];
}

// The same for a failure before a task exists (no token): only a timer is left, and it runs while the app is awake.
- (void)retry_later:(NSString*)text {
    self.failures++;
    note([NSString stringWithFormat:@"failed (%d): %@", self.failures, text]);
    if (self.failures >= kRetries) { [self stop:text]; return; }
    self.active = true;  // the chain waits here: nothing else starts one meanwhile
    self.current = nil;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(MIN(60, 5 << self.failures) * NSEC_PER_SEC)), self.queue, ^{
        if (!self.halted && self.active && !self.current) [self step];
    });
}

// Append a finished chunk to its file, at the offset it was asked for.
- (NSString*)append:(NSURL*)tmp path:(NSString*)path offset:(int64_t)offset status:(NSInteger)status {
    NSString* dst = [partial_path() stringByAppendingPathComponent:path];
    NSFileManager* fm = [NSFileManager defaultManager];
    [fm createDirectoryAtPath:[dst stringByDeletingLastPathComponent] withIntermediateDirectories:YES attributes:nil error:nil];
    if (status == 200 && offset > 0) return @"Dropbox did not resume the file.";
    if (![fm fileExistsAtPath:dst]) [fm createFileAtPath:dst contents:nil attributes:nil];
    NSFileHandle* out = [NSFileHandle fileHandleForWritingAtPath:dst];
    NSFileHandle* in = [NSFileHandle fileHandleForReadingFromURL:tmp error:nil];
    if (!out || !in) return @"Could not write the game's files.";
    NSError* e = nil;
    unsigned long long len = 0;
    if (![out seekToEndReturningOffset:&len error:&e]) return e.localizedDescription;
    if (status == 200) { [out truncateAtOffset:0 error:nil]; len = 0; offset = 0; }  // the whole file came: start over
    if ((int64_t)len > offset) { [out truncateAtOffset:(unsigned long long)offset error:nil]; len = (unsigned long long)offset; }
    if ((int64_t)len < offset) return @"A piece of the game's file is missing: starting that file again.";
    [out seekToOffset:len error:nil];
    for (;;) {
        @autoreleasepool {
            NSData* d = [in readDataUpToLength:4 << 20 error:&e];
            if (!d.length) break;
            if (![out writeData:d error:&e]) return e.localizedDescription ?: @"Could not write the game's files.";
        }
    }
    [out closeFile];
    [in closeFile];
    return nil;
}

#pragma mark NSURLSessionDownloadDelegate

- (void)URLSession:(NSURLSession*)s downloadTask:(NSURLSessionDownloadTask*)t didWriteData:(int64_t)n totalBytesWritten:(int64_t)w
    totalBytesExpectedToWrite:(int64_t)x {
    dispatch_async(self.queue, ^{
        if (!self.current || self.current.unsignedIntegerValue != t.taskIdentifier) return;
        self.inflight = w;
        show_progress([self status_text], self.total ? (double)(self.base + w) / self.total : 0);
    });
}

// The temporary file is deleted when this returns: appended now, on self.queue so that no step (which hashes and
// deletes files) runs meanwhile. Errors are judged in didCompleteWithError.
- (void)URLSession:(NSURLSession*)s downloadTask:(NSURLSessionDownloadTask*)t didFinishDownloadingToURL:(NSURL*)tmp {
    NSInteger status = [(NSHTTPURLResponse*)t.response statusCode];
    NSString* path = nil;
    int64_t offset = 0, length = 0, size = 0;
    if (!parse_description(t.taskDescription, &path, &offset, &length, &size)) return;
    dispatch_sync(self.queue, ^{
        if (![self claim:t]) { note(@"a late chunk of an older download was dropped"); return; }
        NSString* problem = nil;
        if (status == 200 || status == 206) {
            problem = [self append:tmp path:path offset:offset status:status];
        } else if (status == 401) {
            problem = @"Dropbox refused the sign-in.";  // usually a token that expired while the task waited: retried
        } else if (status == 409 || status == 404) {
            problem = [NSString stringWithFormat:@"The game's files are not in Dropbox yet (%@ in save-relay's app folder, acnh-universal-game/ios/).", path];
        } else {
            problem = [NSString stringWithFormat:@"Dropbox answered %ld.", (long)status];
        }
        self.chunk_error = problem;
    });
}

- (void)URLSession:(NSURLSession*)s task:(NSURLSessionTask*)t didCompleteWithError:(NSError*)error {
    // Time to finish the step when the system relaunched the app in the background; ended when it runs out (a hash of a
    // big file can take longer: the step then waits until the app is opened, but the app is not killed for it).
    UIApplication* app = UIApplication.sharedApplication;
    __block UIBackgroundTaskIdentifier bg = UIBackgroundTaskInvalid;
    bg = [app beginBackgroundTaskWithName:@"game download" expirationHandler:^{
        [app endBackgroundTask:bg];
        bg = UIBackgroundTaskInvalid;
    }];
    dispatch_async(self.queue, ^{
        if ([self claim:t]) {
            self.current = nil;
            NSString* problem = self.chunk_error;
            self.chunk_error = nil;
            if (error && !problem) problem = error.localizedDescription;
            if (problem) {
                if ([problem hasPrefix:@"The game's files are not in Dropbox"])
                    [self stop:problem];
                else
                    [self retry:problem];
            } else {
                self.failures = 0;
                NSString* path;
                int64_t offset, length, size;
                // A file's last chunk landed: if its hash is wrong the plan lists the file from 0 again, which step() counts.
                if (parse_description(t.taskDescription, &path, &offset, &length, &size) && offset + length >= size)
                    self.fetched[path] = @(self.fetched[path].unsignedIntegerValue + 1);
                [self step];
            }
        } else {
            note(@"a task of an older download ended: ignored");
        }
        if (bg != UIBackgroundTaskInvalid) [app endBackgroundTask:bg];
    });
}

- (void)URLSessionDidFinishEventsForBackgroundURLSession:(NSURLSession*)s {
    void (^done)(void);
    @synchronized(background_lock()) { done = g_background_done; g_background_done = nil; }
    if (done) dispatch_async(dispatch_get_main_queue(), done);
}

@end
namespace {

// ---- application:handleEventsForBackgroundURLSession:completionHandler: --------------------------------------------

// SDL owns the app delegate, which does not implement this one: it is added to its class when the app loads, before
// UIApplicationMain makes the delegate (UIKit asks a delegate what it implements when it is set).
void handle_events(id, SEL, UIApplication*, NSString* identifier, void (^completion)(void)) {
    if (![identifier isEqualToString:kSessionId]) { completion(); return; }
    note(@"the system relaunched the app for the game download");
    // The session exists again, so its events are delivered; the delegate runs the chain from them (a claimed task
    // finishing starts the next chunk), with no game thread: SDL_main does not run until a scene connects.
    @synchronized(background_lock()) { g_background_done = [completion copy]; }  // before the session is made
    [NXGameDownloader shared];
}

__attribute__((constructor)) void install_background_hook() {
    // SDL 3 on iOS 13 and later makes the scene delegate the app's delegate class (SDLUIKitSceneDelegate); older paths use
    // SDLUIKitDelegate. Add it to every one that exists: UIKit asks only the class it instantiated.
    bool added = false;
    for (NSString* name in @[ @"SDLUIKitSceneDelegate", @"SDLUIKitDelegate", @"SDLAppDelegate" ]) {
        Class c = NSClassFromString(name);
        if (!c) continue;
        SEL sel = @selector(application:handleEventsForBackgroundURLSession:completionHandler:);
        if (class_addMethod(c, sel, (IMP)handle_events, "v@:@@@?")) added = true;
    }
    // Not found: a download still resumes the next time the app is opened, from what is on disk.
    if (!added) note(@"no app delegate to receive background session events");
}

// ---- the first-start screen --------------------------------------------------------------------------------------

// Signed in, or false when the person gave up.
bool sign_in() {
    while (!acnh_appgame_signed_in()) {
        int go = ask(@"Download the game",
                     @"The game waits in your own Dropbox. Sign in to Dropbox: Safari opens the sign-in page. Allow access, "
                     @"copy the code it shows, then come back here and paste it.",
                     @[ @"Open Dropbox", @"Not now" ]);
        if (go != 0) return false;
        NSDictionary* start = take(acnh_appgame_sign_in_start());
        if (!ok(start)) {
            if (ask(@"Dropbox", why(start), @[ @"Try again", @"Not now" ]) != 0) return false;
            continue;
        }
        NSURL* url = [NSURL URLWithString:start[@"url"]];
        dispatch_async(dispatch_get_main_queue(), ^{ [UIApplication.sharedApplication openURL:url options:@{} completionHandler:nil]; });
        NSString* code = @"";
        int pasted = ask(@"Dropbox code", @"Paste the code Dropbox showed you.", @[ @"Sign in", @"Not now" ], &code, @"Code");
        if (pasted != 0) return false;
        NSDictionary* done = take(acnh_appgame_sign_in_finish(code.UTF8String));
        if (!ok(done) && ask(@"Dropbox", why(done), @[ @"Try again", @"Not now" ]) != 0) return false;
    }
    return true;
}

// The file list: Dropbox's, always when it can be reached (the owner may have prepared the game again since a download
// began; the plan then drops what no longer matches). With no network the one a download in progress kept is used, so
// a resume works offline. nil when the person gave up.
NSData* load_manifest(bool resuming) {
    NSData* kept = resuming ? [NSData dataWithContentsOfFile:pending_path()] : nil;
    for (;;) {
        NSDictionary* tok = take(acnh_appgame_token());
        NSString* problem = nil;
        if (ok(tok)) {
            NSMutableURLRequest* r = [NSMutableURLRequest requestWithURL:[NSURL URLWithString:kRemoteHost]];
            r.HTTPMethod = @"POST";
            [r setValue:[@"Bearer " stringByAppendingString:tok[@"token"]] forHTTPHeaderField:@"Authorization"];
            [r setValue:[[NXGameDownloader shared] raw_api_arg:@"manifest.json"] forHTTPHeaderField:@"Dropbox-API-Arg"];
            r.timeoutInterval = 30;
            dispatch_semaphore_t got = dispatch_semaphore_create(0);
            __block NSData* body = nil;
            __block NSInteger status = 0;
            __block NSString* net = nil;
            NSURLSession* quick = [NSURLSession sessionWithConfiguration:NSURLSessionConfiguration.ephemeralSessionConfiguration];
            [[quick dataTaskWithRequest:r completionHandler:^(NSData* d, NSURLResponse* resp, NSError* e) {
                body = d;
                status = [(NSHTTPURLResponse*)resp statusCode];
                net = e.localizedDescription;
                dispatch_semaphore_signal(got);
            }] resume];
            dispatch_semaphore_wait(got, DISPATCH_TIME_FOREVER);
            if (status == 200 && body) return body;
            if (kept && status == 0) { note(@"no network: resuming with the kept file list"); return kept; }
            if (status == 409 || status == 404)
                problem = @"The game's files are not in Dropbox yet. On your Mac run tools/package_ios.sh with --small (it "
                          @"writes an acnh-universal-game folder), copy that folder into Dropbox/Apps/save-relay/, wait for Dropbox to "
                          @"finish uploading, then try again.";
            else if (status == 401)
                problem = @"Dropbox refused the sign-in.";
            else
                problem = net ?: [NSString stringWithFormat:@"Dropbox answered %ld.", (long)status];
            if (status == 401) acnh_appgame_sign_out();
        } else if ([tok[@"reauth"] boolValue]) {
            if (!sign_in()) return nil;
            continue;
        } else {
            problem = why(tok);
        }
        if (ask(@"Download the game", problem, @[ @"Try again", @"Not now" ]) != 0) return nil;
    }
}

}  // namespace

// Called on the game's thread before the game boots, when the game root has no analysis/main.bin. True when the game is
// now installed under Application Support/game; false when the person said Not now or the download stopped.
static bool first_start_impl() {
    @autoreleasepool {
        wait_until_active();  // a background launch has nobody to ask, and the game must not boot there
        NXGameDownloader* d = [NXGameDownloader shared];  // first: events of a download from before reach the delegate
        // A kept file list means the person already said yes (it is written only after the question below).
        bool resuming = [[NSFileManager defaultManager] fileExistsAtPath:pending_path()] && acnh_appgame_signed_in();
        for (;;) {
            if (!acnh_appgame_signed_in() && !sign_in()) return false;
            NSData* manifest = load_manifest(resuming);
            if (!manifest) return false;
            NSDictionary* plan = take(acnh_appgame_plan(support_root().UTF8String, (const uint8_t*)manifest.bytes, manifest.length,
                                                        kAbi, kBuilds, free_bytes()));
            if (!ok(plan)) {
                ask(@"Download the game", why(plan), @[ @"OK" ]);
                return false;
            }
            if ([plan[@"installed"] boolValue]) { wait_until_active(); return true; }
            exclude_from_backup(partial_path());
            if (!resuming) {
                double total = [plan[@"total"] doubleValue], remaining = [plan[@"remaining"] doubleValue];
                NSString* question = [NSString stringWithFormat:@"Download the game (about %@)?", gb(total)];
                NSString* detail = [NSString stringWithFormat:@"%@ to download over Wi-Fi, %@ free on this device. It goes on in the "
                                    @"background and picks up where it stopped if it is interrupted.", gb(remaining), gb((double)free_bytes())];
                if (ask(question, detail, @[ @"Download", @"Not now" ]) != 0) return false;
            }
            [manifest writeToFile:pending_path() atomically:YES];  // the yes (or the newer list of a download in progress)
            resuming = true;  // Try again goes on without asking again
            dispatch_semaphore_t finished = [d begin];
            show_progress(@"Starting", 0);
            dispatch_semaphore_wait(finished, DISPATCH_TIME_FOREVER);
            dispatch_async(dispatch_get_main_queue(), ^{ close_window(); });
            NSString* failure = d.failure;
            if (!failure) { wait_until_active(); return true; }
            if (ask(@"Download the game", failure, @[ @"Try again", @"Not now" ]) != 0) {
                [[NSFileManager defaultManager] removeItemAtPath:pending_path() error:nil];  // the next launch asks again
                return false;
            }
        }
    }
}

extern "C" bool nx_appgame_first_start() {
    bool r = first_start_impl();
    // Whatever the outcome, the black alert-level window must not stay over the game (or the sync prompt after this).
    dispatch_async(dispatch_get_main_queue(), ^{ close_window(); });
    return r;
}

// Where the game is: platform_mac.cpp's game root looks here when the bundle has no game/.
extern "C" const char* nx_appgame_game_dir() {
    static std::string dir;
    if (dir.empty()) dir = std::string(game_path().UTF8String);
    return dir.c_str();
}

// Whether the downloaded game (Application Support/game/manifest.json) is of the build and runtime this app was compiled
// for. game/ outlives an app update and a SideStore re-sign, and the game's code is linked into the player, so
// platform_mac.cpp asks at every start and offers the matching download (nx_appgame_first_start) when it is not.
extern "C" bool nx_appgame_game_matches() {
    return acnh_appgame_installed_ok(support_root().UTF8String, kAbi, kBuilds);
}
