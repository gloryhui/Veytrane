(($, request, action) => {
 const panel = $('filePanel');
 const path = $('path');
 const taskList = $('taskList');
 let active = false;
 panel.hidden = true;

 function setActive(next) {
  next = Boolean(next);
  if (next === active) return false;
  active = next;
  panel.hidden = !active;
  if (!active) {
   path.value = '';
   taskList.replaceChildren();
  }
  return true;
 }

 async function loadTasks(offset = 0) {
  if (!active) return;
  const value = await request({ method: 'tasks', offset });
  if (!active) return;
  taskList.replaceChildren();
  for (const task of value.tasks) {
   const row = document.createElement('p');
   const copy = document.createElement('span');
   const actions = document.createElement('span');
   row.className = 'task-row';
   copy.className = 'task-copy';
   actions.className = 'row-actions';
   copy.textContent = task.name + ' · ' + task.state + ' · ' + task.confirmed + '/' + task.total;
   for (const [label, method] of [['暂停', 'pause'], ['继续', 'resume'], ['取消', 'cancel']]) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'row-action';
    button.textContent = label;
    button.onclick = () => action(async () => {
     await request({
      method,
      task: task.id,
      ...(method === 'resume' ? { peer: task.peer } : {}),
     });
     await loadTasks();
    });
    actions.append(button);
   }
   row.append(copy, actions);
   taskList.append(row);
  }
  for (const preview of value.directory_previews || []) {
   const row = document.createElement('p');
   const copy = document.createElement('span');
   const actions = document.createElement('span');
   row.className = 'task-row';
   copy.className = 'task-copy';
   actions.className = 'row-actions';
   copy.textContent = '目录预览：新增' + preview.added + '，变化' + preview.changed + '，不变' + preview.unchanged;
   for (const [label, method] of [['确认目录', 'confirm_directory'], ['取消预览', 'cancel_directory']]) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'row-action';
    button.textContent = label;
    button.onclick = () => action(async () => {
     await request({
      method,
      request: preview.id,
      ...(method === 'confirm_directory' ? { peer: preview.peer } : {}),
     });
     await loadTasks(offset);
    });
    actions.append(button);
   }
   row.append(copy, actions);
   taskList.append(row);
  }
  if (value.next_offset !== null) {
   const wrapper = document.createElement('div');
   const next = document.createElement('button');
   wrapper.className = 'task-pager';
   next.type = 'button';
   next.className = 'row-action';
   next.textContent = '下一页';
   next.onclick = () => action(() => loadTasks(value.next_offset));
   wrapper.append(next);
   taskList.append(wrapper);
  }
  if (value.truncated) {
   const note = document.createElement('p');
   note.className = 'task-note';
   note.textContent = '任务超过当前页上限，请使用CLI查看。';
   taskList.append(note);
  }
 }

 $('send').onclick = () => action(() => request({
  method: 'send',
  peer: $('peer').value.trim(),
  paths: [path.value],
 }));
 $('preview').onclick = () => action(() => request({
  method: 'preview_directory',
  peer: $('peer').value.trim(),
  path: path.value,
 }));
 $('tasks').onclick = () => action(loadTasks);

 return Object.freeze({
  setActive,
  permissionColumns() {
   return active ? [['文件', 'files', 'bool']] : [];
  },
  get active() {
   return active;
  },
 });
})($, request, action)
