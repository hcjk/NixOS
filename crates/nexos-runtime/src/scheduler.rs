#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    Ready,
    Running,
    Sleeping { wake_tick: u64 },
    Blocked,
    Exited { status: i32 },
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuContext {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub instruction_pointer: u64,
    pub stack_pointer: u64,
    pub flags: u64,
    pub address_space: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub process_id: u32,
    pub state: TaskState,
    pub context: CpuContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    Full,
    NoCurrentTask,
    TaskNotFound,
    InvalidQuantum,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleDecision {
    Continue(TaskId),
    Switch {
        previous: Option<TaskId>,
        next: TaskId,
    },
    Idle,
}

pub struct Scheduler<const TASKS: usize> {
    tasks: [Option<Task>; TASKS],
    next_id: u32,
    current: Option<usize>,
    quantum_ticks: u32,
    remaining_ticks: u32,
    context_switches: u64,
}

impl<const TASKS: usize> Scheduler<TASKS> {
    pub fn new(quantum_ticks: u32) -> Result<Self, SchedulerError> {
        if quantum_ticks == 0 {
            return Err(SchedulerError::InvalidQuantum);
        }
        Ok(Self::with_quantum(quantum_ticks))
    }

    #[must_use]
    pub const fn with_quantum(quantum_ticks: u32) -> Self {
        Self {
            tasks: [None; TASKS],
            next_id: 1,
            current: None,
            quantum_ticks,
            remaining_ticks: quantum_ticks,
            context_switches: 0,
        }
    }

    pub fn spawn(
        &mut self,
        process_id: u32,
        context: CpuContext,
    ) -> Result<TaskId, SchedulerError> {
        let slot = self
            .tasks
            .iter()
            .position(Option::is_none)
            .ok_or(SchedulerError::Full)?;
        let id = TaskId(self.next_id);
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.tasks[slot] = Some(Task {
            id,
            process_id,
            state: TaskState::Ready,
            context,
        });
        Ok(id)
    }

    #[must_use]
    pub fn current(&self) -> Option<Task> {
        self.current.and_then(|index| self.tasks[index])
    }

    #[must_use]
    pub fn task(&self, id: TaskId) -> Option<Task> {
        self.tasks
            .iter()
            .flatten()
            .find(|task| task.id == id)
            .copied()
    }

    #[must_use]
    pub fn task_count(&self) -> usize {
        self.tasks.iter().flatten().count()
    }

    #[must_use]
    pub const fn context_switches(&self) -> u64 {
        self.context_switches
    }

    pub fn schedule(&mut self, now: u64) -> ScheduleDecision {
        self.wake_sleepers(now);
        let previous = self.current.and_then(|index| self.tasks[index]);
        if let Some(index) = self.current
            && let Some(task) = &mut self.tasks[index]
            && task.state == TaskState::Running
        {
            task.state = TaskState::Ready;
        }

        let start = self.current.map_or(0, |index| (index + 1) % TASKS.max(1));
        let next = self.find_ready_from(start);
        let Some(next_index) = next else {
            self.current = None;
            return ScheduleDecision::Idle;
        };

        let next_task = self.tasks[next_index].as_mut().expect("ready task exists");
        next_task.state = TaskState::Running;
        let next_id = next_task.id;
        self.current = Some(next_index);
        self.remaining_ticks = self.quantum_ticks;

        if previous.map(|task| task.id) == Some(next_id) {
            ScheduleDecision::Continue(next_id)
        } else {
            self.context_switches = self.context_switches.saturating_add(1);
            ScheduleDecision::Switch {
                previous: previous.map(|task| task.id),
                next: next_id,
            }
        }
    }

    pub fn tick(&mut self, now: u64) -> ScheduleDecision {
        self.wake_sleepers(now);
        let Some(current) = self.current else {
            return self.schedule(now);
        };
        let Some(task) = self.tasks[current] else {
            self.current = None;
            return self.schedule(now);
        };
        if task.state != TaskState::Running {
            return self.schedule(now);
        }
        self.remaining_ticks = self.remaining_ticks.saturating_sub(1);
        if self.remaining_ticks == 0 {
            self.schedule(now)
        } else {
            ScheduleDecision::Continue(task.id)
        }
    }

    pub fn sleep_current(&mut self, wake_tick: u64) -> Result<TaskId, SchedulerError> {
        let index = self.current.ok_or(SchedulerError::NoCurrentTask)?;
        let task = self.tasks[index]
            .as_mut()
            .ok_or(SchedulerError::NoCurrentTask)?;
        task.state = TaskState::Sleeping { wake_tick };
        self.current = None;
        Ok(task.id)
    }

    pub fn block_current(&mut self) -> Result<TaskId, SchedulerError> {
        let index = self.current.ok_or(SchedulerError::NoCurrentTask)?;
        let task = self.tasks[index]
            .as_mut()
            .ok_or(SchedulerError::NoCurrentTask)?;
        task.state = TaskState::Blocked;
        self.current = None;
        Ok(task.id)
    }

    pub fn wake(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        let task = self
            .tasks
            .iter_mut()
            .flatten()
            .find(|task| task.id == id)
            .ok_or(SchedulerError::TaskNotFound)?;
        if matches!(task.state, TaskState::Blocked | TaskState::Sleeping { .. }) {
            task.state = TaskState::Ready;
        }
        Ok(())
    }

    pub fn exit_current(&mut self, status: i32) -> Result<TaskId, SchedulerError> {
        let index = self.current.ok_or(SchedulerError::NoCurrentTask)?;
        let task = self.tasks[index]
            .as_mut()
            .ok_or(SchedulerError::NoCurrentTask)?;
        task.state = TaskState::Exited { status };
        self.current = None;
        Ok(task.id)
    }

    pub fn exit_task(&mut self, id: TaskId, status: i32) -> Result<(), SchedulerError> {
        let task = self
            .tasks
            .iter_mut()
            .flatten()
            .find(|task| task.id == id)
            .ok_or(SchedulerError::TaskNotFound)?;
        task.state = TaskState::Exited { status };
        if self
            .current
            .is_some_and(|index| self.tasks[index].is_some_and(|task| task.id == id))
        {
            self.current = None;
        }
        Ok(())
    }

    pub fn reap(&mut self, id: TaskId) -> Result<Task, SchedulerError> {
        let slot = self
            .tasks
            .iter()
            .position(|task| task.is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::TaskNotFound)?;
        let task = self.tasks[slot].ok_or(SchedulerError::TaskNotFound)?;
        if !matches!(task.state, TaskState::Exited { .. }) {
            return Err(SchedulerError::TaskNotFound);
        }
        self.tasks[slot] = None;
        Ok(task)
    }

    fn wake_sleepers(&mut self, now: u64) {
        for task in self.tasks.iter_mut().flatten() {
            if matches!(task.state, TaskState::Sleeping { wake_tick } if wake_tick <= now) {
                task.state = TaskState::Ready;
            }
        }
    }

    fn find_ready_from(&self, start: usize) -> Option<usize> {
        if TASKS == 0 {
            return None;
        }
        (0..TASKS)
            .map(|offset| (start + offset) % TASKS)
            .find(|index| self.tasks[*index].is_some_and(|task| task.state == TaskState::Ready))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().flatten()
    }
}

pub struct WaitQueue<const WAITERS: usize> {
    waiters: [Option<TaskId>; WAITERS],
}

impl<const WAITERS: usize> WaitQueue<WAITERS> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            waiters: [None; WAITERS],
        }
    }

    pub fn push(&mut self, task: TaskId) -> Result<(), SchedulerError> {
        let slot = self
            .waiters
            .iter_mut()
            .find(|waiter| waiter.is_none())
            .ok_or(SchedulerError::Full)?;
        *slot = Some(task);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<TaskId> {
        let index = self.waiters.iter().position(Option::is_some)?;
        let task = self.waiters[index];
        for position in index..WAITERS.saturating_sub(1) {
            self.waiters[position] = self.waiters[position + 1];
        }
        if WAITERS != 0 {
            self.waiters[WAITERS - 1] = None;
        }
        task
    }
}

impl<const WAITERS: usize> Default for WaitQueue<WAITERS> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_robin_preempts_at_quantum_boundary() {
        let mut scheduler = Scheduler::<4>::new(2).unwrap();
        let first = scheduler.spawn(1, CpuContext::default()).unwrap();
        let second = scheduler.spawn(2, CpuContext::default()).unwrap();
        assert!(matches!(
            scheduler.schedule(0),
            ScheduleDecision::Switch { next, .. } if next == first
        ));
        assert_eq!(scheduler.tick(1), ScheduleDecision::Continue(first));
        assert!(matches!(
            scheduler.tick(2),
            ScheduleDecision::Switch { previous: Some(old), next }
                if old == first && next == second
        ));
    }

    #[test]
    fn sleeping_task_wakes_at_requested_tick() {
        let mut scheduler = Scheduler::<2>::new(1).unwrap();
        let task = scheduler.spawn(1, CpuContext::default()).unwrap();
        scheduler.schedule(0);
        scheduler.sleep_current(5).unwrap();
        assert_eq!(scheduler.schedule(4), ScheduleDecision::Idle);
        assert!(matches!(
            scheduler.schedule(5),
            ScheduleDecision::Switch { next, .. } if next == task
        ));
    }

    #[test]
    fn wait_queue_is_fifo() {
        let mut queue = WaitQueue::<2>::new();
        queue.push(TaskId(4)).unwrap();
        queue.push(TaskId(7)).unwrap();
        assert_eq!(queue.pop(), Some(TaskId(4)));
        assert_eq!(queue.pop(), Some(TaskId(7)));
        assert_eq!(queue.pop(), None);
    }
}
