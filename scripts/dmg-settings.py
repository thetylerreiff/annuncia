# dmgbuild settings for Annuncia.dmg: the app and an Applications link on the background in
# assets/dmg, which draws the arrow between them. scripts/dmg.sh passes the paths as defines.
#
# Finder always draws icon labels in black on a custom background, even in Dark Mode, so the
# icons sit on the light part of it.

app = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)
background = defines["background"]  # noqa: F821
icon = defines["icon"]  # noqa: F821

files = [app]
symlinks = {"Applications": "/Applications"}
hide_extensions = ["Annuncia.app"]

format = "ULFO"
filesystem = "HFS+"

# The height includes the 32 pt title bar, so all 400 pt of the background show.
window_rect = ((200, 160), (640, 432))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
include_icon_view_settings = True
arrange_by = None
icon_size = 96
text_size = 13
label_pos = "bottom"
icon_locations = {"Annuncia.app": (190, 285), "Applications": (450, 285)}
