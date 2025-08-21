# EISENPOWER - Eisenhower Matrix with a current-day priority list : Rust + Axum + SQLite (+ optional Docker)

This is a modified form of the Eisenhower Matrix method for creating TODO lists according to urgency and important. I also created another panel for ordering the tasks that are currently on the docket for the day/week.

Several years ago I developed a version of this in Swift for MacOS and iOS, but that wasn't portable across devices, and wasn't web accessible. It also didn't track completed items and allow you to pull them out of the completed bin if you realized you weren't done yer.

Here are some of the features

- Drag & drop between columns to reorder/move.
- When dragging to the Today's Task list, it preserves which category it came from, in case you want to move it back to where it came from.
- Add tasks via the input at the bottom of each column.
- Click a task's text to edit. Automatically saved when navigating away.
- Checkmark button to indicate the task is done and move it to the Completed Tasks list
- X button to delete a task entirely.
- In the Completed Tasks panel, click the restore button to move the item back to the list of items still needing to be done.
- Data lives in a local file `tasks.db` (created automatically for the docker image, and the git repository contains and empty tasks.db with all the right tables).
- Requires rudimentary HTTP auth to log in (default username is 'admin' and default password is 'password'). These can be changed via the docker-compose.yml file or environment variables.


*Note*: This was 100% done via vibe coding, using my Swift implementation from 8 years ago and a screenshot of that interface as the input. I consider myself a pretty good programmer, but this was an experiment about whether I could make a fully functioning app without actually doing any coding. During this whole process, I resisted the urge to jump in and make code corrections along the way. Instead I was using GPT4.1 and only giving it prompts about what features I wanted to add, how I was expecting it to behave, and how it actually behaved. This was in VSCode with Copilot, so it was often interpreting the compiler errors and acting accordingly. I have used AI a lot in my day-to-day coding, but never done the full 'vibe coding' thing. I usually just have it implement a function here or there where I am giving it a function signature or a data structure or a directive, then edit code, then have it optimize or improve, etc. This was wholly code written by it without any "coding intervention" by me.

## Screenshots
![The main window](images/main_window.png)

![The completed tasks modal window](images/completed_tasks.png)


## How to Run
To run from the command line:
```
cargo run
```
OR
```
cargo run --release
```

To run by creating a docker image and container
```
docker compose up --build
```

Then open http://127.0.0.1:8080  (or whatever IP address you chose and whatever IP you chose, if using docker)

Enjoy!


