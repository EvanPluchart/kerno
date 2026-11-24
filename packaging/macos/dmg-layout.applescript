-- Arranges the Finder window of a mounted Kerno disk image, which Finder then keeps in the volume's
-- .DS_Store. Run by packaging/macos/make-dmg-layout.sh:
--   osascript dmg-layout.applescript <mount point>
-- The window shows the 640 x 400 background: its bounds hold the 28-point title bar too. The
-- icons' positions are their centres, the places the background's arrow is drawn between.
on run argv
	set mountPoint to item 1 of argv
	tell application "Finder"
		with timeout of 60 seconds
			set theDisk to (POSIX file mountPoint) as alias
			open theDisk
			set theWindow to container window of theDisk
			set current view of theWindow to icon view
			set toolbar visible of theWindow to false
			set statusbar visible of theWindow to false
			set the bounds of theWindow to {200, 120, 840, 548}
			set theOptions to the icon view options of theWindow
			set arrangement of theOptions to not arranged
			set icon size of theOptions to 128
			set text size of theOptions to 13
			set label position of theOptions to bottom
			set shows item info of theOptions to false
			set shows icon preview of theOptions to false
			set background picture of theOptions to file ".background:background.tiff" of theDisk
			set position of item "Kerno.app" of theDisk to {160, 180}
			set position of item "Applications" of theDisk to {480, 180}
			-- Closing and opening again makes Finder write the view to .DS_Store.
			close theWindow
			open theDisk
			update theDisk without registering applications
			delay 2
			close (container window of theDisk)
		end timeout
	end tell
end run
