#include <gtk/gtk.h>
#include <gdk/gdkwayland.h>
#include <gtk-layer-shell.h>
#include <wayland-client.h>
#include <math.h>
#include <string.h>
#include "background-effect.h"

/* One resident layer surface; the GTK display owns its connection until exit. */
static struct {
    struct wl_display *display;
    struct wl_event_queue *queue;
    struct wl_registry *registry;
    struct wl_compositor *compositor;
    struct ext_background_effect_manager_v1 *manager;
    struct ext_background_effect_surface_v1 *effect;
    gboolean attempted, supported, globals_attempted;
    gboolean has_rect;
    int rect[4];
} blur;

static void capabilities(void *data, struct ext_background_effect_manager_v1 *manager, uint32_t flags) {
    (void)data; (void)manager;
    blur.supported = (flags & EXT_BACKGROUND_EFFECT_MANAGER_V1_CAPABILITY_BLUR) != 0;
}

static const struct ext_background_effect_manager_v1_listener effect_listener = { capabilities };

static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (!strcmp(interface, "wl_compositor")) {
        blur.compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 1);
    } else if (!strcmp(interface, "ext_background_effect_manager_v1")) {
        blur.manager = wl_registry_bind(registry, name, &ext_background_effect_manager_v1_interface, 1);
        ext_background_effect_manager_v1_add_listener(blur.manager, &effect_listener, NULL);
    }
}

static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}

static const struct wl_registry_listener registry_listener = { global, global_remove };

/* The compositor and background-effect manager, shared by every surface. */
static gboolean init_globals(GdkDisplay *display) {
    if (blur.globals_attempted) return blur.manager && blur.compositor && blur.supported;
    if (!display || !GDK_IS_WAYLAND_DISPLAY(display)) return FALSE;
    blur.globals_attempted = TRUE;
    blur.display = gdk_wayland_display_get_wl_display(display);
    blur.queue = wl_display_create_queue(blur.display);
    if (!blur.queue) return FALSE;
    blur.registry = wl_display_get_registry(blur.display);
    wl_proxy_set_queue((struct wl_proxy *)blur.registry, blur.queue);
    wl_registry_add_listener(blur.registry, &registry_listener, NULL);
    if (wl_display_roundtrip_queue(blur.display, blur.queue) < 0 ||
        wl_display_roundtrip_queue(blur.display, blur.queue) < 0 ||
        !blur.manager || !blur.compositor || !blur.supported) {
        g_warning("keytab: compositor background blur is unavailable");
        return FALSE;
    }
    return TRUE;
}

static gboolean init_blur(GtkWindow *window) {
    if (blur.attempted) return blur.effect != NULL;
    GdkWindow *gdk_window = gtk_widget_get_window(GTK_WIDGET(window));
    if (!gdk_window || !GDK_IS_WAYLAND_DISPLAY(gdk_window_get_display(gdk_window))) return FALSE;
    struct wl_surface *surface = gdk_wayland_window_get_wl_surface(gdk_window);
    if (!surface) return FALSE;
    blur.attempted = TRUE;
    if (!init_globals(gdk_window_get_display(gdk_window))) return FALSE;
    blur.effect = ext_background_effect_manager_v1_get_background_effect(
        blur.manager, surface);
    return blur.effect != NULL;
}

/* A rectangle with rounded corners, approximated by one-pixel rows. */
static struct wl_region *rounded_region(int x, int y, int width, int height, int max_radius) {
    struct wl_region *region = wl_compositor_create_region(blur.compositor);
    if (!region) return NULL;
    int radius = MIN(max_radius, MIN(width / 2, height / 2));
    wl_region_add(region, x, y + radius, width, height - 2 * radius);
    for (int row = 0; row < radius; row++) {
        double dy = radius - row - 0.5;
        int inset = radius - (int)sqrt(radius * radius - dy * dy);
        wl_region_add(region, x + inset, y + row, width - 2 * inset, 1);
        wl_region_add(region, x + inset, y + height - row - 1, width - 2 * inset, 1);
    }
    return region;
}

void keytab_material(GtkWindow *window, int x, int y, int width, int height) {
    if (!init_blur(window)) return;
    wl_display_dispatch_queue_pending(blur.display, blur.queue);
    int rect[] = { x, y, width, height };
    if (blur.has_rect && !memcmp(blur.rect, rect, sizeof rect)) return;
    memcpy(blur.rect, rect, sizeof rect);
    blur.has_rect = TRUE;
    struct wl_region *region = NULL;
    if (width > 0 && height > 0) {
        region = rounded_region(x, y, width, height, 10);
        if (!region) return;
    }
    ext_background_effect_surface_v1_set_blur_region(blur.effect, region);
    if (region) wl_region_destroy(region);
    gtk_layer_try_force_commit(window);
    wl_display_flush(blur.display);
}

void keytab_commit(GtkWindow *window) {
    gtk_layer_try_force_commit(window);
    GdkDisplay *display = gtk_widget_get_display(GTK_WIDGET(window));
    gdk_display_flush(display);
}

/* Whether the compositor can blur behind surfaces (ext-background-effect-v1). */
gboolean keytab_blur_available(void) {
    return init_globals(gdk_display_get_default());
}

/* Blur behind a whole ordinary (xdg) window, such as the settings window. Call it once the
 * window is mapped; keytab_window_blur_release must run when it unmaps, before the
 * wl_surface can be reused, because setting a region on a destroyed surface is fatal. */
void keytab_window_blur(GtkWindow *window, int width, int height, int radius) {
    GdkWindow *gdk_window = gtk_widget_get_window(GTK_WIDGET(window));
    if (!gdk_window || width <= 0 || height <= 0) return;
    if (!init_globals(gdk_window_get_display(gdk_window))) return;
    struct wl_surface *surface = gdk_wayland_window_get_wl_surface(gdk_window);
    if (!surface) return;
    struct ext_background_effect_surface_v1 *effect = g_object_get_data(G_OBJECT(window), "keytab-blur");
    if (!effect) {
        effect = ext_background_effect_manager_v1_get_background_effect(blur.manager, surface);
        if (!effect) return;
        g_object_set_data(G_OBJECT(window), "keytab-blur", effect);
    }
    wl_display_dispatch_queue_pending(blur.display, blur.queue);
    struct wl_region *region = rounded_region(0, 0, width, height, radius);
    if (!region) return;
    ext_background_effect_surface_v1_set_blur_region(effect, region);
    wl_region_destroy(region);
    /* The region applies on the window's next commit. */
    gtk_widget_queue_draw(GTK_WIDGET(window));
    wl_display_flush(blur.display);
}

void keytab_window_blur_release(GtkWindow *window) {
    struct ext_background_effect_surface_v1 *effect = g_object_steal_data(G_OBJECT(window), "keytab-blur");
    if (effect) ext_background_effect_surface_v1_destroy(effect);
}
