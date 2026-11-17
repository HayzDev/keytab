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
    gboolean attempted, supported;
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

static gboolean init_blur(GtkWindow *window) {
    if (blur.attempted) return blur.effect != NULL;
    GdkWindow *gdk_window = gtk_widget_get_window(GTK_WIDGET(window));
    if (!gdk_window || !GDK_IS_WAYLAND_DISPLAY(gdk_window_get_display(gdk_window))) return FALSE;
    struct wl_surface *surface = gdk_wayland_window_get_wl_surface(gdk_window);
    if (!surface) return FALSE;
    blur.attempted = TRUE;
    blur.display = gdk_wayland_display_get_wl_display(gdk_window_get_display(gdk_window));
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
    blur.effect = ext_background_effect_manager_v1_get_background_effect(
        blur.manager, surface);
    return blur.effect != NULL;
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
        region = wl_compositor_create_region(blur.compositor);
        if (!region) return;
        int radius = MIN(10, MIN(width / 2, height / 2));
        wl_region_add(region, x, y + radius, width, height - 2 * radius);
        for (int row = 0; row < radius; row++) {
            double dy = radius - row - 0.5;
            int inset = radius - (int)sqrt(radius * radius - dy * dy);
            wl_region_add(region, x + inset, y + row, width - 2 * inset, 1);
            wl_region_add(region, x + inset, y + height - row - 1, width - 2 * inset, 1);
        }
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
