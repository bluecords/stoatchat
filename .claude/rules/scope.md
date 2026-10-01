# Scope rule - loads only when working in a NAC repo

*Canonical copy in claude-repo. Copied into each NAC repo's `.claude/rules/`. `RULED BY BUNJIE`, 2026-10-01.*

Before calling any build or fix done, answer both without being asked:

1. **The WHOLE unit.** A post is text + images + video + audio + reactions + replies, not just
   its text. Move/copy/sync/export code must handle every part, and you must read what the
   backend does with that data first. (Autumn files attach to ONE message, so a move must
   re-upload them.)
2. **The WHOLE class.** Fixed one card/screen/route? Grep for its siblings and fix or list them
   all. Then run the real path once with the messy case (an image, not just text), against the
   live system, not only the code.

Misses this came from: move feature dropped attachments (404); permission sync synced nothing;
one card fixed and the rest of its class left.
