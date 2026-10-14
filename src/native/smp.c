/* Thin C seam over LG's StarfishMediaAPIs (C++, libplayerAPIs.so.1) for the Rust player.
 *
 * The class is only reachable through its mangled symbols. Everything C++-shaped stays here:
 *  - the object is constructed in place in a buffer we own (its real size is not public; webOS
 *    headers say a few hundred bytes, 64 KiB is generous) and is never new/delete'd;
 *  - Feed() returns std::string (cxx11 ABI on this firmware): the hidden return slot is the first
 *    argument; on ARM32 the string is {char *p; size_t len; union {char buf[16]; size_t cap;}};
 *  - Load() is the callback-with-context overload, so late events carry our session id.
 * Call shapes cross-checked against Kodi's MediaPipelineWebOS and plx-native's starfish.c; the
 * symbol set is from this TV's libplayerAPIs (webOS 10.3.2). */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef void (*smp_cb_t)(int type, long long num, const char *str, void *ctx);

extern void SMP_ctor(void *self, const char *uid) __asm__("_ZN17StarfishMediaAPIsC1EPKc");
extern void SMP_dtor(void *self) __asm__("_ZN17StarfishMediaAPIsD1Ev");
extern int SMP_Load(void *self, const char *payload, smp_cb_t cb, void *ctx)
    __asm__("_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_");
extern void SMP_Feed(void *ret, void *self, const char *payload) __asm__("_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc");
extern int SMP_Play(void *self) __asm__("_ZN17StarfishMediaAPIs4PlayEv");
extern int SMP_Pause(void *self) __asm__("_ZN17StarfishMediaAPIs5PauseEv");
extern int SMP_Unload(void *self) __asm__("_ZN17StarfishMediaAPIs6UnloadEv");
extern int SMP_pushEOS(void *self) __asm__("_ZN17StarfishMediaAPIs7pushEOSEv");
extern int SMP_notifyForeground(void *self) __asm__("_ZN17StarfishMediaAPIs16notifyForegroundEv");

#define SMP_STORAGE (64 * 1024)

/* One allocation per session; freed only by smp_destroy after UNLOADCOMPLETED. */
void *smp_create(void) {
    void *obj = NULL;
    if (posix_memalign(&obj, 16, SMP_STORAGE) != 0) return NULL;
    memset(obj, 0, SMP_STORAGE);
    SMP_ctor(obj, NULL); /* uid NULL: the app's own uMS namespace */
    return obj;
}

int smp_load(void *obj, const char *payload, smp_cb_t cb, void *ctx) {
    SMP_notifyForeground(obj);
    return SMP_Load(obj, payload, cb, ctx);
}

/* Feed one access unit. Returns 'O' accepted, 'B' buffer full (retry later), 'e' error. */
char smp_feed(void *obj, const uint8_t *data, unsigned size, long long pts_ns, int es) {
    char json[160];
    snprintf(json, sizeof json, "{\"bufferAddr\":\"%p\",\"bufferSize\":%u,\"pts\":%lld,\"esData\":%d}",
             (const void *)data, size, pts_ns, es);
    struct { char *p; size_t len; char buf[16]; } ret;
    memset(&ret, 0, sizeof ret);
    SMP_Feed(&ret, obj, json);
    char v = !ret.p ? 'e' : strstr(ret.p, "BufferFull") ? 'B' : strstr(ret.p, "Ok") ? 'O' : 'e';
    if (ret.p && ret.p != ret.buf) {               /* long reply: heap buffer, release it */
        static void (*del)(void *) = NULL;
        if (!del) del = (void (*)(void *))dlsym(RTLD_DEFAULT, "_ZdlPv");
        if (del) del(ret.p);
    }
    return v;
}

int smp_play(void *obj) { return SMP_Play(obj); }
int smp_pause(void *obj) { return SMP_Pause(obj); }
int smp_unload(void *obj) { return SMP_Unload(obj); }
int smp_push_eos(void *obj) { return SMP_pushEOS(obj); }
void smp_destroy(void *obj) { SMP_dtor(obj); free(obj); }

/* ---- the video plane: an exported window the pipeline renders into (webOS 5+) ---- */
typedef struct { int x, y, w, h; } vp_rect;
static const char *(*p_create)(int);
static int (*p_set)(const char *, vp_rect *, vp_rect *);
static void (*p_destroy)(const char *);

static int vp_resolve(void) {
    if (p_create) return 1;
    p_create = (const char *(*)(int))dlsym(RTLD_DEFAULT, "SDL_webOSCreateExportedWindow");
    p_set = (int (*)(const char *, vp_rect *, vp_rect *))dlsym(RTLD_DEFAULT, "SDL_webOSSetExportedWindow");
    p_destroy = (void (*)(const char *))dlsym(RTLD_DEFAULT, "SDL_webOSDestroyExportedWindow");
    return p_create && p_set;
}

/* Copies the compositor-assigned id ("_Window_Id_N") into out; returns 1 on success. */
int vp_create(char *out, int cap) {
    if (!vp_resolve()) return 0;
    const char *id = p_create(0 /* video */);
    if (!id) return 0;
    snprintf(out, cap, "%s", id);
    return 1;
}
int vp_place(const char *id, int sw, int sh, int x, int y, int w, int h) {
    if (!vp_resolve()) return 0;
    vp_rect s = {0, 0, sw, sh}, d = {x, y, w, h};
    return p_set(id, &s, &d);
}
void vp_destroy(const char *id) { if (vp_resolve() && p_destroy) p_destroy(id); }
