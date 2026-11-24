Deleting an item now removes its queued embed jobs. Saving an item enqueues a
`plugin_queue` row under the kernel's reserved `__kernel_embed` identity, with
the item's id only inside the JSON payload, so no foreign key cascaded and the
delete path never touched the queue: the row stayed until the next drain found
the item gone and discarded it. `Item::delete_in` now deletes that item's
`__kernel_embed` rows in the same transaction as the item, and touches no other
plugin's rows.

An enqueue that ran alongside a delete could still leave a job behind, since
it read the item and inserted its job without any lock the delete would see.
`enqueue_embed_job` now holds `FOR KEY SHARE` on the item row while it writes
the job and the item's pending state, and queues nothing (returning `false`)
for an item that is gone; `Item::delete_in` deletes the item row before its
embed jobs. Whichever starts first, the other waits, and no job outlives the
item. A job left over from before this release is still dropped by the drain,
which treats a job whose item no longer exists as done and logs nothing for it.
