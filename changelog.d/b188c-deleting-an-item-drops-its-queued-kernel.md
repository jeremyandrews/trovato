Deleting an item now removes its queued embed jobs. Saving an item enqueues a
`plugin_queue` row under the kernel's reserved `__kernel_embed` identity, with
the item's id only inside the JSON payload, so no foreign key cascaded and the
delete path never touched the queue: the row stayed until the next drain found
the item gone and discarded it. `Item::delete_in` now deletes that item's
`__kernel_embed` rows in the same transaction as the item, and touches no other
plugin's rows. The drain already treats a job whose item no longer exists as
done, and a test now holds that.
