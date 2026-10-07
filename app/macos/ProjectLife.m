// Project Life — the macOS shell.
//
// This program draws the frame only: the window, the menu-bar item, the app menu and the native
// folder panel. Everything else — the interface, the archive, the observation — lives in the two
// binaries beside it (`projectlife-ui`, which serves the interface, and `projectlife`, which the
// interface drives). Closing the window hides it; observation continues. Quitting stops observation
// and says so before it does.
//
// Build: see build_app.sh (clang, the macOS SDK, and the two Rust binaries).

// The owner's name, his address and the sentence about what the program is for. Generated from
// `src/brand.rs` by `tools/brand.py`; the build passes `-I ../` so the same header the Linux and
// Windows shells include is found here.
#include "pl_brand.h"

#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>
#import <errno.h>
#import <fcntl.h>
#import <netinet/in.h>
#import <arpa/inet.h>
#import <sys/socket.h>
#import <sys/select.h>
#import <sys/un.h>
#import <string.h>
#import <unistd.h>

static NSString *const kAppName = @"Project Life";

// ---------------------------------------------------------------------------------------------
// Talking to the local server
//
// Round 295 taught this file a hard lesson: it launched the interface server, watched its stdout for
// one line, and if that line never came it told the person to reinstall the app. On a real Mac the
// server was refused a listening socket by the system, its own explanation went into a pipe nobody
// read, and the window said "reinstall". Three things are different now:
//
//   1. the child's stderr is read and appended to daemon.log, so the system's words survive;
//   2. the reason is carried into the window instead of a generic sentence;
//   3. if the child cannot open a listening socket at all, this process — a different process, an
//      app bundle the system can attribute — binds one and hands the descriptor over the unix socket
//      below. The child never has to call `bind` for the handoff to work.

static NSString *PLStamp(void) {
    NSDateFormatter *f = [[NSDateFormatter alloc] init];
    f.dateFormat = @"yyyy-MM-dd HH:mm:ss";
    return [f stringFromDate:[NSDate date]];
}

/// Is this path a program we may launch?
///
/// A folder is never an answer. On 2026-10-06 the owner measured the second half of this failure on
/// his Mac: the search for the interface server returned nil, the fallback handed the *folder*
/// containing the executables to NSTask, and the app told him to reinstall.
/// A string quoted for JavaScript. Menu labels and ids come from the same place as the data, and an
/// id is a fixed identifier — but a string that reaches `evaluateJavaScript` is quoted properly or
/// not at all.
static NSString *PLJSONString(NSString *s) {
    NSData *d = [NSJSONSerialization dataWithJSONObject:@[ s ?: @"" ] options:0 error:nil];
    NSString *json = [[NSString alloc] initWithData:d encoding:NSUTF8StringEncoding] ?: @"[\"\"]";
    return [json substringWithRange:NSMakeRange(1, json.length - 2)];
}

static BOOL PLIsProgram(NSString *path) {
    if (!path.length) return NO;
    NSFileManager *fm = [NSFileManager defaultManager];
    BOOL isDir = NO;
    if (![fm fileExistsAtPath:path isDirectory:&isDir]) return NO;
    if (isDir) return NO;
    return [fm isExecutableFileAtPath:path];
}

/// Where a helper program lives inside this bundle — and, when it is not found, why every candidate
/// that did not answer is not an answer.
///
/// Round 295 asked the bundle for the resource folder *inside* the resource folder, which
/// is where nothing lives: the resource directory already is `Contents/Resources`.
/// The owner confirmed it with Foundation on the Mac. So the folder is never named a second time,
/// and the result is checked for being a plain executable file rather than trusted by name.
static NSString *PLToolPath(NSString *name, NSMutableArray *tried) {
    NSBundle *b = [NSBundle mainBundle];
    NSFileManager *fm = [NSFileManager defaultManager];
    NSMutableArray *candidates = [NSMutableArray array];
    // 1. the resource directory, spelled out — no extra "Resources" in front of it
    if (b.bundlePath.length) {
        [candidates addObject:[b.bundlePath stringByAppendingPathComponent:
                              [@"Contents/Resources" stringByAppendingPathComponent:name]]];
    }
    // 2. the bundle's own answer to the same question
    NSString *byName = [b pathForResource:name ofType:nil];
    if (byName.length) [candidates addObject:byName];
    // 3. beside the main executable
    NSString *exeDir = [b.executablePath stringByDeletingLastPathComponent];
    if (exeDir.length) [candidates addObject:[exeDir stringByAppendingPathComponent:name]];
    for (NSString *c in candidates) {
        BOOL isDir = NO;
        BOOL exists = [fm fileExistsAtPath:c isDirectory:&isDir];
        if (!exists) {
            [tried addObject:[NSString stringWithFormat:@"%@ — no such file", c]];
            continue;
        }
        if (isDir) {
            [tried addObject:[NSString stringWithFormat:@"%@ — a folder, not a program", c]];
            continue;
        }
        if (![fm isExecutableFileAtPath:c]) {
            [tried addObject:[NSString stringWithFormat:@"%@ — not executable", c]];
            continue;
        }
        [tried addObject:[NSString stringWithFormat:@"%@ — used", c]];
        return c;
    }
    return nil;
}

/// One descriptor and one message in a single send, or a plain message when there is no descriptor.
static ssize_t PLSendMsg(int sock, const void *body, size_t len, int fd, BOOL withFd) {
    struct iovec iov;
    iov.iov_base = (void *)body;
    iov.iov_len = len;
    char cbuf[CMSG_SPACE(sizeof(int))];
    struct msghdr msg;
    memset(&msg, 0, sizeof msg);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    if (withFd) {
        msg.msg_control = cbuf;
        msg.msg_controllen = sizeof cbuf;
        struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fd, sizeof fd);
    }
    return sendmsg(sock, &msg, 0);
}

@interface PLServer : NSObject
@property (nonatomic, strong) NSTask *task;
@property (nonatomic, strong) NSString *url;
@property (nonatomic, strong) NSString *token;
@property (nonatomic) int port;
@property (nonatomic) int ipcFd;
@property (nonatomic, strong) NSString *ipcPath;
@property (nonatomic, strong) NSString *logPath;
@property (nonatomic, strong) NSString *how;            // explicit / ladder / handed-over
@property (nonatomic, strong) NSString *lastErrorText;  // the honest message for the window
@property (nonatomic, strong) NSString *lastState;      // human text for the menu bar
@property (nonatomic, strong) NSString *lastStateLine;
@property (nonatomic, strong) NSString *uiPath;          // the interface server inside this bundle
@property (nonatomic, strong) NSString *corePath;        // the core program inside this bundle
@property (nonatomic, strong) NSString *iconName;        // what the menu-bar shield should be
@property (nonatomic, strong) NSString *buildShort;      // app 0.9.1 · ui 1ea8e883 · core 010669a9
@property (nonatomic, strong) NSString *buildCheck;      // how to check that line from outside
@property (nonatomic) BOOL observationRunning;
@property (nonatomic) BOOL recordingStopped;             // the promise is not being kept right now
- (BOOL)startWithExecutable:(NSString *)exe core:(NSString *)core;
- (void)stop;
- (id)get:(NSString *)path;          // synchronous GET of /api/<path> on the local server
- (NSString *)textAt:(NSString *)path;
- (NSString *)menuText;              // the whole menu, as the server defines it
- (void)post:(NSString *)path body:(NSDictionary *)body;
- (void)refreshState;
@end

@implementation PLServer {
    NSCondition *_cond;
    NSMutableArray *_lines;        // the child's stdout lines, as JSON
    NSMutableString *_stderrTail;  // its last words, for the alert
    BOOL _finished;
    int _exitCode;
    NSFileHandle *_log;
}

- (instancetype)init {
    self = [super init];
    if (self) {
        _cond = [[NSCondition alloc] init];
        _lines = [NSMutableArray array];
        _stderrTail = [NSMutableString string];
        _exitCode = -1;
        self.ipcFd = -1;
    }
    return self;
}

// ---------------------------------------------------------------- the log

/// Everything this app and its child say goes to one file, in order, with the moment it happened.
- (void)appendToLog:(NSString *)text {
    @synchronized(self) {
        if (!_log) {
            if (![[NSFileManager defaultManager] fileExistsAtPath:self.logPath]) {
                [[NSFileManager defaultManager] createFileAtPath:self.logPath contents:nil attributes:nil];
            }
            _log = [NSFileHandle fileHandleForWritingAtPath:self.logPath];
            [_log seekToEndOfFile];
        }
        [_log writeData:[text dataUsingEncoding:NSUTF8StringEncoding]];
    }
}

- (void)note:(NSString *)line {
    NSString *text = [NSString stringWithFormat:@"%@ ProjectLife: %@\n", PLStamp(), line];
    fprintf(stderr, "%s", text.UTF8String);
    [self appendToLog:text];
}

/// The child's own words, kept in the same file. It stamps its own lines, so this appends them
/// unchanged — no double timestamps, nothing rewritten.
- (void)captureChildText:(NSString *)text {
    if (!text.length) return;
    [self appendToLog:text];
    @synchronized(self) {
        [_stderrTail appendString:text];
        if (_stderrTail.length > 8192) {
            [_stderrTail deleteCharactersInRange:NSMakeRange(0, _stderrTail.length - 8192)];
        }
    }
}

// ---------------------------------------------------------------- the handoff socket

- (BOOL)openHandoffSocket:(NSString *)path {
    unlink(path.fileSystemRepresentation);
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        [self note:[NSString stringWithFormat:@"cannot create the handoff socket: %s (errno %d)",
                                              strerror(errno), errno]];
        return NO;
    }
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    struct sockaddr_un sun;
    memset(&sun, 0, sizeof sun);
    sun.sun_family = AF_UNIX;
    strlcpy(sun.sun_path, path.fileSystemRepresentation, sizeof sun.sun_path);
    if (bind(fd, (struct sockaddr *)&sun, sizeof sun) != 0) {
        [self note:[NSString stringWithFormat:@"cannot bind the handoff socket %@: %s (errno %d)",
                                              path, strerror(errno), errno]];
        close(fd);
        return NO;
    }
    if (listen(fd, 1) != 0) {
        [self note:[NSString stringWithFormat:@"cannot listen on the handoff socket: %s (errno %d)",
                                              strerror(errno), errno]];
        close(fd);
        return NO;
    }
    self.ipcFd = fd;
    self.ipcPath = path;
    return YES;
}

/// Bind a loopback port in *this* process, recording what the system said about every address.
- (int)bindLadderFrom:(int)first count:(int)count port:(int *)outPort said:(NSMutableString *)said {
    for (int i = 0; i < count; i++) {
        int p = first + i;
        if (p <= 0 || p > 65535) continue;
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) {
            [said appendFormat:@"socket(): %s (errno %d); ", strerror(errno), errno];
            return -1;
        }
        int one = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
        struct sockaddr_in a;
        memset(&a, 0, sizeof a);
        a.sin_family = AF_INET;
        a.sin_port = htons((uint16_t)p);
        a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (bind(fd, (struct sockaddr *)&a, sizeof a) == 0 && listen(fd, 128) == 0) {
            *outPort = p;
            return fd;
        }
        [said appendFormat:@"127.0.0.1:%d refused: %s (errno %d); ", p, strerror(errno), errno];
        close(fd);
    }
    return -1;
}

/// The child cannot open a socket. It has said so, with the addresses already refused; this process
/// answers on the handoff socket — with a descriptor it bound itself, or with its own refusal.
- (BOOL)answerNeedSocket:(NSDictionary *)need {
    NSMutableString *said = [NSMutableString string];
    int want = [need[@"port"] intValue];
    if (want <= 0) want = 7717;
    int port = 0;
    int sfd = [self bindLadderFrom:want count:10 port:&port said:said];
    BOOL ok = (sfd >= 0);
    [self note:[NSString stringWithFormat:@"the interface server asked this app for a socket; its own attempts: %@%@",
            said, ok ? [NSString stringWithFormat:@"— this app bound 127.0.0.1:%d", port]
                     : @"— this app could not bind a local port either"]];

    if (self.ipcFd < 0) {
        [self note:@"there is no handoff socket to answer on"];
        if (ok) close(sfd);
        return NO;
    }
    fd_set set;
    FD_ZERO(&set);
    FD_SET(self.ipcFd, &set);
    struct timeval tv;
    tv.tv_sec = 15;
    tv.tv_usec = 0;
    int cfd = -1;
    if (select(self.ipcFd + 1, &set, NULL, NULL, &tv) > 0) {
        cfd = accept(self.ipcFd, NULL, NULL);
    }
    if (cfd < 0) {
        [self note:[NSString stringWithFormat:@"the interface server did not connect to the handoff socket: %s",
                                              strerror(errno)]];
        if (ok) close(sfd);
        return NO;
    }
    char buf[512];
    ssize_t n = read(cfd, buf, sizeof buf - 1);
    if (n < 0) n = 0;
    buf[n] = 0;
    [self note:[NSString stringWithFormat:@"the interface server asked: %s", buf]];

    if (ok) {
        const char *grant = "{\"grant\":true}\n";
        ssize_t sent = PLSendMsg(cfd, grant, strlen(grant), sfd, YES);
        [self note:(sent > 0
                      ? [NSString stringWithFormat:@"handed 127.0.0.1:%d to the interface server", port]
                      : [NSString stringWithFormat:@"cannot hand the socket over: %s (errno %d)",
                                                          strerror(errno), errno])];
        close(sfd);
        close(cfd);
        return sent > 0;
    }
    NSDictionary *d = @{ @"grant": @NO,
                         @"reason": [NSString stringWithFormat:@"this app could not bind a local port either: %@", said] };
    NSData *body = [NSJSONSerialization dataWithJSONObject:d options:0 error:nil];
    ssize_t sent = PLSendMsg(cfd, body.bytes, body.length, -1, NO);
    close(cfd);
    (void)sent;
    return NO;
}

// ---------------------------------------------------------------- starting the child

- (NSDictionary *)waitForLine:(NSTimeInterval)timeout {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:timeout];
    [_cond lock];
    while (_lines.count == 0 && !_finished) {
        if (![_cond waitUntilDate:deadline]) break;
    }
    NSDictionary *line = nil;
    if (_lines.count > 0) {
        NSData *raw = [_lines.firstObject dataUsingEncoding:NSUTF8StringEncoding];
        [_lines removeObjectAtIndex:0];
        id j = [NSJSONSerialization JSONObjectWithData:raw options:0 error:nil];
        if ([j isKindOfClass:[NSDictionary class]]) line = j;
    }
    [_cond unlock];
    return line;
}

- (void)readStdout:(int)fd {
    NSMutableData *buf = [NSMutableData data];
    char tmp[4096];
    for (;;) {
        ssize_t n = read(fd, tmp, sizeof tmp);
        if (n <= 0) break;
        [buf appendBytes:tmp length:(NSUInteger)n];
        for (;;) {
            NSRange nl = [buf rangeOfData:[@"\n" dataUsingEncoding:NSUTF8StringEncoding]
                                  options:0 range:NSMakeRange(0, buf.length)];
            if (nl.location == NSNotFound) break;
            NSData *one = [buf subdataWithRange:NSMakeRange(0, nl.location)];
            NSString *text = [[NSString alloc] initWithData:one encoding:NSUTF8StringEncoding];
            [buf replaceBytesInRange:NSMakeRange(0, nl.location + 1) withBytes:NULL length:0];
            if (text.length) {
                [_cond lock];
                [_lines addObject:text];
                [_cond broadcast];
                [_cond unlock];
            }
        }
    }
}

- (void)readStderr:(int)fd {
    char tmp[4096];
    for (;;) {
        ssize_t n = read(fd, tmp, sizeof tmp);
        if (n <= 0) break;
        NSString *text = [[NSString alloc] initWithBytes:tmp length:(NSUInteger)n encoding:NSUTF8StringEncoding];
        if (!text) text = [[NSString alloc] initWithBytes:tmp length:(NSUInteger)n encoding:NSISOLatin1StringEncoding];
        [self captureChildText:text];
    }
}

- (BOOL)startWithExecutable:(NSString *)exe core:(NSString *)core {
    NSString *support = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/ProjectLife"];
    [[NSFileManager defaultManager] createDirectoryAtPath:support withIntermediateDirectories:YES attributes:nil error:nil];
    self.logPath = [support stringByAppendingPathComponent:@"daemon.log"];
    [self note:@"--- Project Life.app starting the interface server"];

    BOOL handoff = [self openHandoffSocket:[support stringByAppendingPathComponent:@"ipc.sock"]];
    if (!handoff) {
        [self note:@"the handoff socket is unavailable; the interface server will have to bind its own port"];
    }

    // An explicit local port first, then its neighbours, then the kernel's own choice — the same
    // order the child would use, so a busy port is a busy port in both processes.
    NSMutableArray *args = [NSMutableArray arrayWithObjects:exe, @"--pl", core, @"--port", @"7717",
                            @"--port-range", @"10", @"--native", @"--log-dir", support, nil];
    if (handoff) {
        [args addObjectsFromArray:@[ @"--ipc", self.ipcPath ]];
    } else {
        [args addObject:@"--no-ipc"];
    }

    NSTask *t = [[NSTask alloc] init];
    t.executableURL = [NSURL fileURLWithPath:exe];
    t.arguments = [args subarrayWithRange:NSMakeRange(1, args.count - 1)];
    NSPipe *out = [NSPipe pipe];
    NSPipe *err = [NSPipe pipe];
    t.standardOutput = out;
    t.standardError = err;
    __weak PLServer *weakSelf = self;
    t.terminationHandler = ^(NSTask *task) {
        PLServer *me = weakSelf;
        if (!me) return;
        [me->_cond lock];
        me->_finished = YES;
        me->_exitCode = task.terminationStatus;
        [me->_cond broadcast];
        [me->_cond unlock];
        [me note:[NSString stringWithFormat:@"the interface server exited with status %d", (int)task.terminationStatus]];
    };
    NSError *err2 = nil;
    if (![t launchAndReturnError:&err2]) {
        self.lastErrorText = [NSString stringWithFormat:
            @"The interface server could not be started at all:\n%@\n\n%@\n%@",
            err2.localizedDescription, exe, self.logPath];
        [self note:[NSString stringWithFormat:@"launch failed: %@", err2.localizedDescription]];
        return NO;
    }
    self.task = t;
    [self note:[NSString stringWithFormat:@"interface server pid %d: %@", (int)t.processIdentifier,
                                          [t.arguments componentsJoinedByString:@" "]]];

    int outFd = out.fileHandleForReading.fileDescriptor;
    int errFd = err.fileHandleForReading.fileDescriptor;
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{ [self readStdout:outFd]; });
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{ [self readStderr:errFd]; });

    NSDictionary *line = [self waitForLine:20.0];
    if (line && line[@"needSocket"]) {
        // The child is waiting for a socket from us. It says so before it gives up, so this is a
        // handoff, not a rescue.
        [self answerNeedSocket:line];
        line = [self waitForLine:25.0];
    }
    if (!line) {
        NSString *why = _finished
            ? [NSString stringWithFormat:@"The interface server stopped before it could listen (exit status %d).", _exitCode]
            : @"The interface server did not report a port within 20 seconds.";
        self.lastErrorText = [self failureText:why detail:nil];
        return NO;
    }
    if (line[@"error"]) {
        self.lastErrorText = [self failureTextFromJSON:line[@"error"]];
        return NO;
    }
    self.port = [line[@"port"] intValue];
    self.token = line[@"token"];
    self.url = line[@"url"];
    self.how = line[@"how"];
    [self note:[NSString stringWithFormat:@"listening on 127.0.0.1:%d (%@)", self.port, self.how ?: @"?"]];
    return self.port > 0;
}

// ---------------------------------------------------------------- the honest failure

- (NSString *)failureText:(NSString *)headline detail:(NSDictionary *)err {
    NSMutableString *m = [NSMutableString string];
    [m appendFormat:@"%@\n\n", headline];
    if (err) {
        if ([err[@"reason"] isKindOfClass:[NSString class]]) {
            [m appendFormat:@"The system said: %@\n\n", err[@"reason"]];
        }
        id attempts = err[@"attempts"];
        if ([attempts isKindOfClass:[NSArray class]] && [attempts count]) {
            [m appendString:@"Addresses tried by this app's own server:\n"];
            NSUInteger shown = MIN((NSUInteger)4, [attempts count]);
            for (NSUInteger i = 0; i < shown; i++) {
                NSDictionary *a = attempts[i];
                [m appendFormat:@"  %@ — %@\n", a[@"addr"], a[@"detail"]];
            }
            if ([attempts count] > shown) {
                [m appendFormat:@"  … and %lu more, all listed in the log\n", (unsigned long)([attempts count] - shown)];
            }
            [m appendString:@"\n"];
        }
        if ([err[@"advice"] isKindOfClass:[NSString class]]) {
            [m appendFormat:@"%@\n\n", err[@"advice"]];
        }
        if ([err[@"handoff"] isKindOfClass:[NSString class]]) {
            [m appendFormat:@"Handing the socket over: %@\n\n", err[@"handoff"]];
        }
    }
    NSString *tail = nil;
    @synchronized(self) {
        tail = [_stderrTail copy];
    }
    if (tail.length) {
        [m appendString:@"Last words of the interface server:\n"];
        NSArray *lines = [tail componentsSeparatedByString:@"\n"];
        NSUInteger from = lines.count > 6 ? lines.count - 6 : 0;
        for (NSUInteger i = from; i < lines.count; i++) {
            if ([lines[i] length]) [m appendFormat:@"  %@\n", lines[i]];
        }
        [m appendString:@"\n"];
    }
    [m appendFormat:@"Everything is written here:\n%@\n\n", self.logPath];
    [m appendString:@"To see what this machine allows, run this in Terminal and send me the file it writes:\n"];
    if (self.uiPath.length) {
        [m appendFormat:@"\"%@\" --diagnose\n", self.uiPath];
    } else {
        [m appendString:@"(the interface server was not found inside this app bundle; the bundle is incomplete)\n"];
    }
    return m;
}

- (NSString *)failureTextFromJSON:(NSDictionary *)err {
    return [self failureText:@"The interface server could not open a listening socket, so there is nothing for the window to talk to."
                      detail:err];
}

- (void)stop {
    if (self.task && self.task.isRunning) {
        [self.task terminate];           // SIGTERM: the server stops the observation it started
        NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:8.0];
        while (self.task.isRunning && [deadline timeIntervalSinceNow] > 0) {
            [NSThread sleepForTimeInterval:0.1];
        }
        if (self.task.isRunning) [self.task interrupt];
    }
    if (self.ipcFd >= 0) {
        close(self.ipcFd);
        self.ipcFd = -1;
    }
    if (self.ipcPath) unlink(self.ipcPath.fileSystemRepresentation);
}

// ---------------------------------------------------------------- requests

- (NSURL *)apiURL:(NSString *)path {
    return [NSURL URLWithString:[NSString stringWithFormat:@"http://127.0.0.1:%d/api/%@", self.port, path]];
}

- (NSMutableURLRequest *)request:(NSString *)path method:(NSString *)method {
    NSMutableURLRequest *r = [NSMutableURLRequest requestWithURL:[self apiURL:path]];
    r.HTTPMethod = method;
    r.timeoutInterval = 8.0;
    [r setValue:self.token forHTTPHeaderField:@"X-PL-Token"];
    return r;
}

- (id)get:(NSString *)path {
    NSMutableURLRequest *r = [self request:path method:@"GET"];
    __block id result = nil;
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    [[[NSURLSession sharedSession] dataTaskWithRequest:r
        completionHandler:^(NSData *data, NSURLResponse *resp, NSError *e) {
            if (data) result = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
            dispatch_semaphore_signal(sem);
        }] resume];
    dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(9.0 * NSEC_PER_SEC)));
    return result;
}

- (void)post:(NSString *)path body:(NSDictionary *)body {
    NSMutableURLRequest *r = [self request:path method:@"POST"];
    [r setValue:@"application/json" forHTTPHeaderField:@"Content-Type"];
    r.HTTPBody = [NSJSONSerialization dataWithJSONObject:(body ?: @{}) options:0 error:nil];
    __block id result = nil;
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    [[[NSURLSession sharedSession] dataTaskWithRequest:r
        completionHandler:^(NSData *data, NSURLResponse *resp, NSError *e) {
            if (data) result = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
            dispatch_semaphore_signal(sem);
        }] resume];
    dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(9.0 * NSEC_PER_SEC)));
    (void)result;
}

- (NSString *)textAt:(NSString *)path {
    NSMutableURLRequest *r = [self request:path method:@"GET"];
    __block NSString *result = nil;
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    [[[NSURLSession sharedSession] dataTaskWithRequest:r
        completionHandler:^(NSData *data, NSURLResponse *resp, NSError *e) {
            if (data) result = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
            dispatch_semaphore_signal(sem);
        }] resume];
    dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(9.0 * NSEC_PER_SEC)));
    return result;
}

/// The menu as the server defines it. The shell does not keep its own list of what the app can do:
/// it draws whatever this answers, so the menu bar cannot offer something the window cannot do, and
/// a new entry appears here the moment the server knows it. `shell=1` includes the few entries only
/// a native shell can perform (quitting, closing the window, opening the documentation).
- (NSString *)menuText {
    return [self textAt:@"menu?shell=1"];
}

/// What the menu bar shows. One request, and the words come from the app's own state: the mode the
/// core reports, whether observation was started here, and — since 2026-10-06 — whether anything is
/// actually being *written*. On the owner's Mac the shield said "Protected" while the daemon was
/// refusing every write, because a live process was mistaken for a kept promise.
- (void)refreshState {
    NSDictionary *b = [self get:@"watch"];
    if (![b isKindOfClass:[NSDictionary class]]) {
        self.lastState = @"Unavailable";
        self.lastStateLine = @"The interface is not answering.";
        self.observationRunning = NO;
        self.recordingStopped = NO;
        self.iconName = @"shield.slash";
        return;
    }
    // Which build is this? Kept from the server's own answer so the About panel and the menu can
    // both say it: the owner once spent a round reporting on a bundle that had been replaced.
    NSDictionary *bd = [b[@"build"] isKindOfClass:[NSDictionary class]] ? b[@"build"] : nil;
    if (bd) {
        self.buildShort = bd[@"short"];
        self.buildCheck = bd[@"check"];
    }
    self.observationRunning = [b[@"runningByApp"] boolValue];
    NSDictionary *p = [b[@"protection"] isKindOfClass:[NSDictionary class]] ? b[@"protection"] : nil;
    if (p) {
        NSString *state = p[@"state"] ?: @"unknown";
        self.recordingStopped = ![p[@"protected"] boolValue];
        self.lastState = p[@"label"] ?: state;
        self.lastStateLine = p[@"reason"] ?: @"";
        if ([state isEqualToString:@"protected"]) self.iconName = @"checkmark.shield";
        else if ([state isEqualToString:@"protected_low_space"]) self.iconName = @"checkmark.shield";
        else if ([state isEqualToString:@"paused_full"]) self.iconName = @"exclamationmark.triangle";
        else self.iconName = @"shield.slash";
        return;
    }
    // An app server that predates the protection block: say the little that can be said, without
    // claiming protection on the strength of a process being alive.
    NSDictionary *hb = [b[@"heartbeat"] isKindOfClass:[NSDictionary class]] ? b[@"heartbeat"] : nil;
    BOOL fresh = hb ? [hb[@"fresh"] boolValue] : NO;
    NSNumber *age = hb ? hb[@"ageMs"] : nil;
    NSString *seen = age ? [NSString stringWithFormat:@"Last check %.0f s ago.", [age doubleValue] / 1000.0]
                         : @"Nothing has ever been observed.";
    self.recordingStopped = !fresh;
    if (fresh && self.observationRunning) {
        self.lastState = @"Observing";
        self.lastStateLine = seen;
        self.iconName = @"checkmark.shield";
    } else {
        self.lastState = @"Not protecting";
        self.lastStateLine = [NSString stringWithFormat:@"Observation is not reporting. %@", seen];
        self.iconName = @"shield.slash";
    }
}
@end

// ---------------------------------------------------------------------------------------------
// The pl:// scheme: the window asks the shell for a native folder panel or to reveal a folder.
// Keeping this out of the web layer is deliberate: the app never draws its own file browser.

@interface PLSchemeHandler : NSObject <WKURLSchemeHandler>
@property (nonatomic, weak) id delegate;
@end

@implementation PLSchemeHandler

- (void)webView:(WKWebView *)webView startURLSchemeTask:(id<WKURLSchemeTask>)task {
    NSURL *u = task.request.URL;
    NSString *host = u.host ?: @"";
    NSString *action = u.path.length > 1 ? [u.path substringFromIndex:1] : host;
    NSDictionary *q = [self query:u.query];
    NSDictionary *payload = nil;

    if ([action isEqualToString:@"pick-folder"] || [host isEqualToString:@"pick-folder"]) {
        payload = [self runPickerWithTitle:q[@"title"]];
    } else if ([action isEqualToString:@"reveal"]) {
        NSString *path = q[@"path"] ?: @"";
        if (path.length) {
            [[NSWorkspace sharedWorkspace] selectFile:path inFileViewerRootedAtPath:@""];
            payload = @{ @"ok": @YES };
        } else {
            payload = @{ @"ok": @NO, @"error": @"no path" };
        }
    } else {
        payload = @{ @"ok": @NO, @"error": @"unknown action" };
    }

    NSData *data = [NSJSONSerialization dataWithJSONObject:payload options:0 error:nil];
    NSHTTPURLResponse *resp = [[NSHTTPURLResponse alloc] initWithURL:u statusCode:200
        HTTPVersion:@"HTTP/1.1" headerFields:@{ @"Content-Type": @"application/json",
                                                @"Access-Control-Allow-Origin": @"*" }];
    [task didReceiveResponse:resp];
    [task didReceiveData:data];
    [task didFinish];
}

- (void)webView:(WKWebView *)webView stopURLSchemeTask:(id<WKURLSchemeTask>)task { (void)task; }

- (NSDictionary *)query:(NSString *)query {
    NSMutableDictionary *out = [NSMutableDictionary dictionary];
    for (NSString *pair in [query componentsSeparatedByString:@"&"]) {
        NSRange eq = [pair rangeOfString:@"="];
        if (eq.location == NSNotFound) continue;
        NSString *k = [pair substringToIndex:eq.location];
        NSString *v = [pair substringFromIndex:eq.location + 1];
        out[[k stringByRemovingPercentEncoding]] = [v stringByRemovingPercentEncoding];
    }
    return out;
}

- (NSDictionary *)runPickerWithTitle:(NSString *)title {
    NSOpenPanel *panel = [NSOpenPanel openPanel];
    panel.canChooseFiles = NO;
    panel.canChooseDirectories = YES;
    panel.allowsMultipleSelection = NO;
    panel.canCreateDirectories = YES;
    panel.message = @"Project Life protects only a folder you choose.";
    panel.prompt = @"Choose";
    if ([title length]) panel.message = title;
    NSInteger r = [panel runModal];
    if (r != NSModalResponseOK || !panel.URL) return @{ @"path": @"" };
    return @{ @"path": panel.URL.path };
}
@end

// ---------------------------------------------------------------------------------------------
// The app

@interface PLApp : NSObject <NSApplicationDelegate, NSMenuDelegate, WKUIDelegate>
@property (nonatomic, strong) PLServer *server;
@property (nonatomic, strong) NSWindow *window;
@property (nonatomic, strong) WKWebView *web;
@property (nonatomic, strong) NSStatusItem *statusItem;
@property (nonatomic, strong) PLSchemeHandler *schemes;
@property (nonatomic, strong) NSString *menuText;   // the last menu the server answered with
@property (nonatomic) BOOL quitting;
@property (nonatomic) BOOL warnedAboutClose;
@end

@implementation PLApp

- (void)applicationDidFinishLaunching:(NSNotification *)note {
    (void)note;
    NSMutableArray *triedUi = [NSMutableArray array];
    NSMutableArray *triedCore = [NSMutableArray array];
    NSString *exe = PLToolPath(@"projectlife-ui", triedUi);
    NSString *core = PLToolPath(@"projectlife", triedCore);
    if (!exe || !core) {
        // An incomplete bundle is its own failure, and it says exactly which paths it looked at and
        // what it found there. This is the case that told the owner to "reinstall": the paths are
        // the answer, the sentence was not.
        NSMutableString *detail = [NSMutableString string];
        [detail appendString:@"A program this app needs is missing from the bundle, so there is nothing to draw an interface from.\n\n"];
        [detail appendFormat:@"The interface server (%@):\n", exe ? @"found" : @"NOT found"];
        for (NSString *t in triedUi) [detail appendFormat:@"  %@\n", t];
        [detail appendFormat:@"\nThe core program (%@):\n", core ? @"found" : @"NOT found"];
        for (NSString *t in triedCore) [detail appendFormat:@"  %@\n", t];
        [detail appendString:@"\nThis is a packaging fault, not a setting: reinstalling the same bundle would find the same absence."];
        NSAlert *a = [[NSAlert alloc] init];
        a.messageText = @"Project Life could not start: its programs are not where the bundle says they are";
        a.informativeText = detail;
        [a runModal];
        [NSApp terminate:nil];
        return;
    }

    self.server = [[PLServer alloc] init];
    self.server.uiPath = exe;
    self.server.corePath = core;
    if (![self.server startWithExecutable:exe core:core]) {
        // The reason, not a shrug: what the system said, what was tried, where the log is, and the
        // one command that turns the next report into a measurement.
        NSAlert *a = [[NSAlert alloc] init];
        a.messageText = @"Project Life could not start its interface server";
        a.informativeText = self.server.lastErrorText ?: @"No reason was reported; see the log.";
        [a addButtonWithTitle:@"Open the log"];
        [a addButtonWithTitle:@"OK"];
        NSInteger choice = [a runModal];
        if (choice == NSAlertFirstButtonReturn && self.server.logPath) {
            [[NSWorkspace sharedWorkspace] selectFile:self.server.logPath
                             inFileViewerRootedAtPath:[self.server.logPath stringByDeletingLastPathComponent]];
        }
        [NSApp terminate:nil];
        return;
    }

    [self buildMenus];
    [self buildWindow];
    [self buildStatusItem];

    NSURL *url = [NSURL URLWithString:self.server.url];
    [self.web loadRequest:[NSURLRequest requestWithURL:url]];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];

    // The interface may stop the observation or start it; the menu bar follows within a few seconds.
    [NSTimer scheduledTimerWithTimeInterval:3.0 target:self selector:@selector(tick:) userInfo:nil repeats:YES];
}

- (void)tick:(NSTimer *)timer {
    (void)timer;
    [self.server refreshState];
    [self updateStatusItemTitle];
    // The menu follows the window: its language is chosen there, and opening a project turns
    // entries on. The comparison is on the server's own answer, so nothing else can drift.
    [self rebuildMenusIfChanged];
}

// ---------------------------------------------------------------- window

- (void)buildWindow {
    NSRect frame = NSMakeRect(0, 0, 1180, 820);
    self.window = [[NSWindow alloc] initWithContentRect:frame
        styleMask:(NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable |
                   NSWindowStyleMaskResizable)
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = kAppName;
    self.window.minSize = NSMakeSize(940, 640);
    [self.window center];
    self.window.releasedWhenClosed = NO;

    WKWebViewConfiguration *cfg = [[WKWebViewConfiguration alloc] init];
    self.schemes = [[PLSchemeHandler alloc] init];
    [cfg setURLSchemeHandler:self.schemes forURLScheme:@"pl"];

    self.web = [[WKWebView alloc] initWithFrame:frame configuration:cfg];
    self.web.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    // JavaScript's own dialogs need a delegate. Without one, `window.prompt` returns null at once:
    // the owner picked an export folder to import and nothing happened, not even a message.
    self.web.UIDelegate = self;
    self.window.contentView = self.web;

    [[NSNotificationCenter defaultCenter] addObserver:self selector:@selector(windowWillClose:)
        name:NSWindowWillCloseNotification object:self.window];
}

/// Closing the window does not stop anything. The first time it happens, say so once and plainly:
/// the user must never have to guess whether closing a window ended the protection.
- (void)windowWillClose:(NSNotification *)note {
    (void)note;
    [self.server refreshState];
    if (self.warnedAboutClose) return;
    self.warnedAboutClose = YES;
    if (!self.server.observationRunning) return;
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = @"The window is closed — observation continues";
    a.informativeText = @"Project Life keeps saving versions while it is running. Reopen the window from "
                         @"the shield in the menu bar. Quitting from the menu stops observation.";
    [a addButtonWithTitle:@"OK"];
    [a runModal];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)app { (void)app; return NO; }

- (void)showWindow {
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
}

// ---------------------------------------------------------------- menus
//
// The menu bar is *drawn from the server's answer* (`GET /api/menu?shell=1`), not from a list typed
// here. Two things this buys, both of them the point of round 299: an entry that exists in the core
// exists in the menu the moment the server reports it, and an entry cannot be shown here unless the
// server knows how to perform it. The only hand-written parts are the standard Edit and Window
// menus — AppKit's own roles — and the two sentences the shell says when the server does not answer.
//
// Choosing an entry sends its id to the window (`window.plMenu(id)`), so the menu bar, the ⌘K
// palette and the window's own menu bar all end up in the same code path in the page.

- (NSDictionary *)menuDocument {
    NSString *text = [self.server menuText];
    if (!text.length) return nil;
    id parsed = [NSJSONSerialization JSONObjectWithData:[text dataUsingEncoding:NSUTF8StringEncoding]
                                               options:0 error:nil];
    return [parsed isKindOfClass:[NSDictionary class]] ? parsed : nil;
}

/// "meta+K" becomes a macOS key equivalent. No entry gets a chord here that it does not declare.
- (void)applyKey:(NSString *)key toItem:(NSMenuItem *)item {
    if (![key isKindOfClass:[NSString class]] || !key.length) return;
    if (![key hasPrefix:@"meta+"]) return;
    NSString *rest = [key substringFromIndex:5];
    if (!rest.length) return;
    item.keyEquivalent = [rest lowercaseString];
    NSUInteger mask = NSEventModifierFlagCommand;
    unichar c = [rest characterAtIndex:0];
    if ([[NSCharacterSet uppercaseLetterCharacterSet] characterIsMember:c] || c == '?') {
        mask |= NSEventModifierFlagShift;
    }
    item.keyEquivalentModifierMask = mask;
}

- (NSMenuItem *)menuItemFor:(NSDictionary *)entry {
    NSString *label = entry[@"label"] ?: entry[@"id"] ?: @"";
    NSMenuItem *mi = [[NSMenuItem alloc] initWithTitle:label action:@selector(menuRun:) keyEquivalent:@""];
    mi.target = self;
    mi.representedObject = entry;
    [self applyKey:entry[@"key"] toItem:mi];
    mi.enabled = [entry[@"enabled"] boolValue];
    NSString *note = entry[@"note"] ?: @"";
    NSString *run = entry[@"coreRun"] ?: @"";
    BOOL asked = [entry[@"enabled"] boolValue];
    mi.toolTip = asked ? [NSString stringWithFormat:@"%@\n%@", run, note]
                       : [NSString stringWithFormat:@"%@\n%@", run, entry[@"why"] ?: @""];
    if (!asked) mi.submenu = nil;
    return mi;
}

- (void)addGroup:(NSDictionary *)group toMenu:(NSMenu *)menu {
    NSArray *items = [group[@"items"] isKindOfClass:[NSArray class]] ? group[@"items"] : @[];
    for (NSDictionary *entry in items) {
        if (![entry isKindOfClass:[NSDictionary class]]) continue;
        // The shell's own acts (quit, close, the documentation folder) live where macOS puts them:
        // Quit in the application menu, Close in File, the documentation in Help. They are added
        // there by name, so they are not duplicated here.
        if ([entry[@"place"] isEqualToString:@"shell"]) continue;
        [menu addItem:[self menuItemFor:entry]];
    }
}

- (NSMenu *)standardEditMenu {
    NSMenu *editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    [editMenu addItemWithTitle:@"Cut" action:@selector(cut:) keyEquivalent:@"x"];
    [editMenu addItemWithTitle:@"Copy" action:@selector(copy:) keyEquivalent:@"c"];
    [editMenu addItemWithTitle:@"Paste" action:@selector(paste:) keyEquivalent:@"v"];
    [editMenu addItemWithTitle:@"Select all" action:@selector(selectAll:) keyEquivalent:@"a"];
    return editMenu;
}

- (NSMenu *)standardWindowMenu {
    NSMenu *windowMenu = [[NSMenu alloc] initWithTitle:@"Window"];
    [windowMenu addItemWithTitle:@"Minimise" action:@selector(performMiniaturize:) keyEquivalent:@"m"];
    [windowMenu addItemWithTitle:@"Zoom" action:@selector(performZoom:) keyEquivalent:@""];
    windowMenu.itemArray.firstObject.target = nil;
    return windowMenu;
}

- (void)buildMenus {
    NSMenu *bar = [[NSMenu alloc] init];
    NSDictionary *doc = [self menuDocument];
    self.menuText = doc ? [self.server menuText] : nil;

    NSArray *groups = [doc[@"groups"] isKindOfClass:[NSArray class]] ? doc[@"groups"] : @[];

    // 1. the application menu: the group the server calls "app", with Quit always present.
    NSDictionary *appGroup = nil;
    for (NSDictionary *g in groups) {
        if ([g[@"id"] isEqualToString:@"app"]) { appGroup = g; break; }
    }
    NSMenuItem *appItem = [[NSMenuItem alloc] init];
    appItem.title = kAppName;
    NSMenu *appMenu = [[NSMenu alloc] initWithTitle:kAppName];
    if (appGroup) {
        [self addGroup:appGroup toMenu:appMenu];
    } else {
        NSMenuItem *none = [[NSMenuItem alloc]
            initWithTitle:@"The menu could not be read from the app server" action:nil keyEquivalent:@""];
        none.enabled = NO;
        [appMenu addItem:none];
    }
    [appMenu addItem:[NSMenuItem separatorItem]];
    NSMenuItem *quit = [[NSMenuItem alloc]
        initWithTitle:[NSString stringWithFormat:@"Quit %@ completely…", kAppName]
               action:@selector(quitCompletely:) keyEquivalent:@"q"];
    quit.target = self;
    [appMenu addItem:quit];
    appItem.submenu = appMenu;
    [bar addItem:appItem];

    // 2. the rest of the groups, in the server's own order.
    for (NSDictionary *g in groups) {
        if ([g[@"id"] isEqualToString:@"app"]) continue;
        NSMenuItem *holder = [[NSMenuItem alloc] init];
        holder.title = g[@"title"] ?: @"";
        NSMenu *menu = [[NSMenu alloc] initWithTitle:holder.title];
        [self addGroup:g toMenu:menu];
        if ([g[@"id"] isEqualToString:@"file"]) {
            NSMenuItem *closeItem = [[NSMenuItem alloc] initWithTitle:@"Close window"
                                     action:@selector(performClose:) keyEquivalent:@"w"];
            closeItem.target = nil;
            [menu addItem:[NSMenuItem separatorItem]];
            [menu addItem:closeItem];
        }
        if (!menu.numberOfItems) {
            NSMenuItem *none = [[NSMenuItem alloc] initWithTitle:@"Nothing here yet" action:nil keyEquivalent:@""];
            none.enabled = NO;
            [menu addItem:none];
        }
        holder.submenu = menu;
        [bar addItem:holder];
    }

    // 3. AppKit's own roles, and the documentation folder that ships in the bundle.
    NSMenuItem *editItem = [[NSMenuItem alloc] init];
    editItem.submenu = [self standardEditMenu];
    [bar addItem:editItem];

    NSMenuItem *docsItem = [[NSMenuItem alloc] init];
    docsItem.title = @"Help";
    NSMenu *helpMenu = [[NSMenu alloc] initWithTitle:@"Help"];
    NSMenuItem *guide = [[NSMenuItem alloc] initWithTitle:@"Quick guide (in the window)"
                        action:@selector(showHelp) keyEquivalent:@"?"];
    guide.target = self;
    [helpMenu addItem:guide];
    NSMenuItem *docs = [[NSMenuItem alloc] initWithTitle:@"The documentation folder"
                       action:@selector(openDocsFolder) keyEquivalent:@""];
    docs.target = self;
    [helpMenu addItem:docs];
    docsItem.submenu = helpMenu;
    [bar addItem:docsItem];

    NSMenuItem *windowItem = [[NSMenuItem alloc] init];
    NSMenu *windowMenu = [self standardWindowMenu];
    windowItem.submenu = windowMenu;
    [bar addItem:windowItem];
    [NSApp setWindowsMenu:windowMenu];

    NSApp.mainMenu = bar;
}

/// The menu can change under the shell without a restart: the language is chosen in the window, and
/// a project opening turns entries on. The text the server answers with is the thing compared, so no
/// separate bookkeeping can disagree with what is on the menu bar.
- (void)rebuildMenusIfChanged {
    NSString *text = [self.server menuText];
    if (!text.length) return;
    if (self.menuText && [self.menuText isEqualToString:text]) return;
    self.menuText = text;
    [self buildMenus];
    [self rebuildStatusMenu];
}

- (void)menuRun:(NSMenuItem *)sender {
    NSDictionary *entry = sender.representedObject;
    if (![entry isKindOfClass:[NSDictionary class]]) return;
    [self runMenuEntry:entry];
}

- (void)runMenuEntry:(NSDictionary *)entry {
    NSString *itemId = entry[@"id"] ?: @"";
    if ([entry[@"place"] isEqualToString:@"shell"]) {
        if ([itemId isEqualToString:@"app.quit"]) { [self quitCompletely:nil]; return; }
        if ([itemId isEqualToString:@"file.close"]) { [self.window performClose:nil]; return; }
        if ([itemId isEqualToString:@"help.docs"]) { [self openDocsFolder]; return; }
        return;
    }
    [self showWindow];
    if (!self.web) return;
    // The page owns the work: the same function the window's own menu bar and ⌘K call.
    NSString *js = [NSString stringWithFormat:@"window.plMenu && window.plMenu(%@)", PLJSONString(itemId)];
    [self.web evaluateJavaScript:js completionHandler:^(id result, NSError *error) {
        if (error) {
            [self reportMenuFailure:itemId why:error.localizedDescription];
        }
    }];
}

/// If the window cannot perform an entry, the reason is said out loud: a menu that silently does
/// nothing is the failure this whole round exists to prevent.
- (void)reportMenuFailure:(NSString *)itemId why:(NSString *)why {
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = [NSString stringWithFormat:@"“%@” could not be performed", itemId];
    a.informativeText = [NSString stringWithFormat:@"%@\n\nThe window may still be starting. Its log is here:\n%@",
                         why ?: @"no reason was reported", self.server.logPath ?: @"(unknown)"];
    [a addButtonWithTitle:@"Open the window"];
    [a addButtonWithTitle:@"OK"];
    if ([a runModal] == NSAlertFirstButtonReturn) [self showWindow];
}

- (void)showHelp {
    [self showWindow];
    if (!self.web) return;
    [self.web evaluateJavaScript:@"window.plMenu && window.plMenu('help.guide')" completionHandler:nil];
}

/// The documentation that ships inside the bundle, opened in the file manager — the one thing on the
/// Help menu the window cannot do, because it cannot see its own bundle.
- (void)openDocsFolder {
    NSString *res = [NSBundle mainBundle].resourcePath ?: @"";
    NSArray *cands = @[[res stringByAppendingPathComponent:@"docs"], res];
    for (NSString *c in cands) {
        if ([[NSFileManager defaultManager] fileExistsAtPath:c]) {
            [[NSWorkspace sharedWorkspace] openFile:c];
            return;
        }
    }
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = @"The documentation folder is not in this bundle";
    a.informativeText = [NSString stringWithFormat:@"Looked in:\n%@", [cands componentsJoinedByString:@"\n"]];
    [a runModal];
}

- (void)about:(id)sender {
    (void)sender;
    NSString *version = [[NSBundle mainBundle] objectForInfoDictionaryKey:@"CFBundleShortVersionString"];
    [self.server refreshState];
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = [NSString stringWithFormat:@"%@ %@", kAppName, version ?: @"?"];
    NSString *build = self.server.buildShort.length
        ? [NSString stringWithFormat:@"\n\nThis build:\n%@\n%@", self.server.buildShort, self.server.buildCheck ?: @""]
        : @"";
    // Who made it, and what it is for. The words come from `app/pl_brand.h`, which is generated from
    // `src/brand.rs` (the single place the owner's name and address are written) — so the panel, the
    // page, the licence and the Windows version block cannot drift apart.
    NSString *what = [NSString stringWithUTF8String:PL_WHAT_IT_IS];
    NSString *by = [NSString stringWithUTF8String:PL_AUTHOR];
    NSString *mail = [NSString stringWithUTF8String:PL_AUTHOR_EMAIL];
    NSString *lic = [NSString stringWithUTF8String:PL_LICENCE];
    NSString *copy = [NSString stringWithUTF8String:PL_COPYRIGHT];
    NSString *brand = [NSString stringWithFormat:@"%@\n\nBy %@ <%@>\n%@ licence — %@", what, by, mail, lic, copy];
    a.informativeText = [NSString stringWithFormat:@"%@\n\nA local flight recorder for the folders you choose.\n"
                         @"No account, no internet, no telemetry.%@", brand, build];
    [a runModal];
}

- (void)showStatus:(id)sender {
    (void)sender;
    [self.server refreshState];
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = self.server.lastState;
    a.informativeText = [NSString stringWithFormat:@"%@\n\nClosing the window does not stop observation. "
                          @"Quitting does.", self.server.lastStateLine];
    [a runModal];
}

/// One button for the question "why can it not listen here": the interface server's own diagnosis,
/// run in a separate process and written to diagnose.txt next to the log.
- (void)runDiagnosis:(id)sender {
    (void)sender;
    NSString *exe = self.server.uiPath;
    NSString *support = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/ProjectLife"];
    NSString *report = [support stringByAppendingPathComponent:@"diagnose.txt"];
    if (!exe.length) {
        NSAlert *a = [[NSAlert alloc] init];
        a.messageText = @"No diagnosis possible";
        a.informativeText = @"The interface server is missing from this app bundle.";
        [a runModal];
        return;
    }
    NSTask *t = [[NSTask alloc] init];
    t.executableURL = [NSURL fileURLWithPath:exe];
    t.arguments = @[ @"--diagnose", @"--log-dir", support ];
    NSPipe *out = [NSPipe pipe];
    t.standardOutput = out;
    t.standardError = out;
    NSString *text = @"";
    if ([t launchAndReturnError:nil]) {
        NSData *d = [out.fileHandleForReading readDataToEndOfFile];
        [t waitUntilExit];
        text = [[NSString alloc] initWithData:d encoding:NSUTF8StringEncoding] ?: @"";
    }
    NSString *verdict = @"";
    for (NSString *line in [text componentsSeparatedByString:@"\n"]) {
        if ([line hasPrefix:@"verdict:"]) verdict = line;
    }
    [[NSFileManager defaultManager] createFileAtPath:report contents:[text dataUsingEncoding:NSUTF8StringEncoding] attributes:nil];
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = @"Network diagnosis";
    a.informativeText = [NSString stringWithFormat:@"%@\n\nThe full report is written here:\n%@",
                         verdict.length ? verdict : @"The report could not be produced.",
                         report];
    [a addButtonWithTitle:@"Open the report"];
    [a addButtonWithTitle:@"OK"];
    if ([a runModal] == NSAlertFirstButtonReturn) {
        [[NSWorkspace sharedWorkspace] openFile:report];
    }
}

- (void)startObservation:(id)sender {
    (void)sender;
    [self.server post:@"watch/start" body:@{}];
    [self.server refreshState];
    [self updateStatusItemTitle];
}

- (void)stopObservation:(id)sender {
    (void)sender;
    [self.server post:@"watch/stop" body:@{}];
    [self.server refreshState];
    [self updateStatusItemTitle];
}

/// Quitting is the one action that ends the promise, so it asks first and says exactly what stops.
- (void)quitCompletely:(id)sender {
    (void)sender;
    [self.server refreshState];
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = @"Quit Project Life?";
    a.informativeText = self.server.observationRunning
        ? @"Observation will stop when the app quits. Changes made while it is not running are not "
           @"recorded and cannot be restored later. Versions already saved stay in the archive and can "
           @"be restored after you start the app again."
        : @"Observation is already stopped. Versions already saved stay in the archive.";
    [a addButtonWithTitle:@"Quit and stop observation"];
    [a addButtonWithTitle:@"Cancel"];
    if ([a runModal] != NSAlertFirstButtonReturn) return;
    self.quitting = YES;
    [NSApp terminate:nil];
}

// ---------------------------------------------------------------- menu bar item

- (void)buildStatusItem {
    self.statusItem = [[NSStatusBar systemStatusBar] statusItemWithLength:NSVariableStatusItemLength];
    self.statusItem.button.image = [self shieldImage:@"checkmark.shield"];
    self.statusItem.button.toolTip = kAppName;
    NSMenu *menu = [[NSMenu alloc] init];
    menu.delegate = self;
    self.statusItem.menu = menu;
    [self fillStatusMenu:menu];
    [self updateStatusItemTitle];
}

- (NSImage *)shieldImage:(NSString *)name {
    if (@available(macOS 11.0, *)) {
        NSImage *img = [NSImage imageWithSystemSymbolName:name accessibilityDescription:kAppName];
        if (img) return img;
    }
    return nil;   // an empty image leaves just the title
}

- (void)updateStatusItemTitle {
    // The shield follows the same state the window does: a triangle while recording is stopped, a
    // slash while nothing is observing, a tick only when something really is being written.
    self.statusItem.button.image = [self shieldImage:self.server.iconName.length ? self.server.iconName
                                                                               : @"shield.slash"];
    self.statusItem.button.toolTip = [NSString stringWithFormat:@"%@ — %@", kAppName,
                                                                self.server.lastState ?: @"…"];
}

/// Built fresh each time it is opened, so the words are the state as of now.
- (void)menuNeedsUpdate:(NSMenu *)menu {
    [self fillStatusMenu:menu];
}

/// The shield's menu: the state in words, the things a person reaches for without opening the
/// window, and every command the app knows, in the same groups the menu bar uses — both of them
/// read from one answer, so the two cannot disagree.
- (void)fillStatusMenu:(NSMenu *)menu {
    [menu removeAllItems];
    [self.server refreshState];

    NSMenuItem *head = [[NSMenuItem alloc] initWithTitle:self.server.lastState ?: @"—" action:nil keyEquivalent:@""];
    head.enabled = NO;
    [menu addItem:head];
    NSMenuItem *sub = [[NSMenuItem alloc] initWithTitle:self.server.lastStateLine ?: @"" action:nil keyEquivalent:@""];
    sub.enabled = NO;
    [menu addItem:sub];
    if (self.server.buildShort.length) {
        NSMenuItem *build = [[NSMenuItem alloc] initWithTitle:[NSString stringWithFormat:@"build %@", self.server.buildShort]
                                                       action:nil keyEquivalent:@""];
        build.enabled = NO;
        build.toolTip = self.server.buildCheck;
        [menu addItem:build];
    }
    [menu addItem:[NSMenuItem separatorItem]];

    NSMenuItem *open = [[NSMenuItem alloc] initWithTitle:@"Open window" action:@selector(showWindow) keyEquivalent:@""];
    open.target = self;
    [menu addItem:open];

    if (self.server.observationRunning) {
        NSMenuItem *stop = [[NSMenuItem alloc] initWithTitle:@"Stop observation" action:@selector(stopObservation:) keyEquivalent:@""];
        stop.target = self;
        [menu addItem:stop];
    } else {
        NSMenuItem *start = [[NSMenuItem alloc] initWithTitle:@"Start observation" action:@selector(startObservation:) keyEquivalent:@""];
        start.target = self;
        [menu addItem:start];
    }

    NSMenuItem *status = [[NSMenuItem alloc] initWithTitle:@"Protection status…" action:@selector(showStatus:) keyEquivalent:@""];
    status.target = self;
    [menu addItem:status];
    NSMenuItem *diag = [[NSMenuItem alloc] initWithTitle:@"Network diagnosis…" action:@selector(runDiagnosis:) keyEquivalent:@""];
    diag.target = self;
    [menu addItem:diag];

    NSDictionary *text = [self menuDocument];
    NSArray *groups = [text[@"groups"] isKindOfClass:[NSArray class]] ? text[@"groups"] : @[];
    if (groups.count) {
        [menu addItem:[NSMenuItem separatorItem]];
        for (NSDictionary *g in groups) {
            NSMenuItem *holder = [[NSMenuItem alloc] init];
            holder.title = g[@"title"] ?: @"";
            NSMenu *sub2 = [[NSMenu alloc] initWithTitle:holder.title];
            [self addGroup:g toMenu:sub2];
            if (!sub2.numberOfItems) continue;
            holder.submenu = sub2;
            [menu addItem:holder];
        }
    } else {
        NSMenuItem *none = [[NSMenuItem alloc] initWithTitle:@"The menu could not be read from the app server"
                                                      action:nil keyEquivalent:@""];
        none.enabled = NO;
        [menu addItem:none];
    }

    [menu addItem:[NSMenuItem separatorItem]];
    NSMenuItem *q = [[NSMenuItem alloc] initWithTitle:@"Quit completely…" action:@selector(quitCompletely:) keyEquivalent:@""];
    q.target = self;
    [menu addItem:q];
}

- (void)rebuildStatusMenu {
    if (self.statusItem.menu) [self fillStatusMenu:self.statusItem.menu];
}

// ---------------------------------------------------------------- JavaScript's own dialogs
//
// The window draws its own dialog for the one place it needs a name from the person (import), and
// these three exist so that no other prompt, alert or confirm can end in silence: without a
// WKUIDelegate the web view answers `window.prompt` with null and shows nothing at all, which is
// precisely what the owner saw when he chose a folder to import.

- (void)webView:(WKWebView *)webView
    runJavaScriptAlertPanelWithMessage:(NSString *)message
                      initiatedByFrame:(WKFrameInfo *)frame
                     completionHandler:(void (^)(void))completionHandler {
    (void)webView;
    (void)frame;
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = kAppName;
    a.informativeText = message.length ? message : @"";
    [a addButtonWithTitle:@"OK"];
    [a runModal];
    completionHandler();
}

- (void)webView:(WKWebView *)webView
    runJavaScriptConfirmPanelWithMessage:(NSString *)message
                        initiatedByFrame:(WKFrameInfo *)frame
                       completionHandler:(void (^)(BOOL))completionHandler {
    (void)webView;
    (void)frame;
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = kAppName;
    a.informativeText = message.length ? message : @"";
    [a addButtonWithTitle:@"OK"];
    [a addButtonWithTitle:@"Cancel"];
    completionHandler([a runModal] == NSAlertFirstButtonReturn);
}

- (void)webView:(WKWebView *)webView
    runJavaScriptTextInputPanelWithPrompt:(NSString *)prompt
                               defaultText:(NSString *)defaultText
                          initiatedByFrame:(WKFrameInfo *)frame
                         completionHandler:(void (^)(NSString *))completionHandler {
    (void)webView;
    (void)frame;
    NSAlert *a = [[NSAlert alloc] init];
    a.messageText = prompt.length ? prompt : kAppName;
    NSTextField *field = [[NSTextField alloc] initWithFrame:NSMakeRect(0, 0, 320, 24)];
    field.stringValue = defaultText ?: @"";
    a.accessoryView = field;
    [a addButtonWithTitle:@"OK"];
    [a addButtonWithTitle:@"Cancel"];
    [a.window makeFirstResponder:field];
    NSInteger r = [a runModal];
    completionHandler(r == NSAlertFirstButtonReturn ? field.stringValue : nil);
}

// ---------------------------------------------------------------- lifecycle

- (void)applicationWillTerminate:(NSNotification *)note {
    (void)note;
    // The server stops the observation it started (SIGTERM), and the app waits for it to finish.
    [self.server stop];
}

- (BOOL)applicationShouldHandleReopen:(NSApplication *)app hasVisibleWindows:(BOOL)flag {
    (void)app; (void)flag;
    [self showWindow];
    return YES;
}

@end

// ---------------------------------------------------------------------------------------------

int main(int argc, const char **argv) {
    (void)argc; (void)argv;
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyRegular];
        PLApp *delegate = [[PLApp alloc] init];
        app.delegate = delegate;
        [app run];
    }
    return 0;
}
